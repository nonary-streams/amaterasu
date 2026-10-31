//! Atomic retirement of action-released roots after authenticated replacement.
use super::*;

/// Missing evidence or remaining references preserve the entire old root.
pub(super) fn compact_replaced_roots(journal: &mut Journal) {
    for (admission, transition, owner, generation) in journal.closed_actions.retired_roots() {
        let _ = retire_one(journal, &admission, &transition, &owner, generation);
    }
}

fn contains_id(value: &Value, id: &str) -> bool {
    match value {
        Value::String(text) => text == id,
        Value::Array(items) => items.iter().any(|item| contains_id(item, id)),
        Value::Object(items) => items.values().any(|item| contains_id(item, id)),
        _ => false,
    }
}

pub(super) fn retire_one(
    journal: &mut Journal,
    admission: &str,
    transition: &str,
    owner: &str,
    generation: u64,
) -> ModuleResult<()> {
    let certified = journal
        .closed_actions
        .historical_root(admission, transition, owner, generation)
        .ok_or("action_root_certificate_missing")?
        .clone();
    let LifecycleTarget::Session { session_id, .. } = &certified.origin else {
        return Err("action_root_target_invalid".into());
    };
    let session = journal
        .sessions
        .get(session_id)
        .ok_or("action_session_missing")?;
    let current = *session
        .resource_generations
        .get(owner)
        .ok_or("action_generation_missing")?;
    if current <= generation || journal.actions.has_any_session_reference(session_id) {
        return Err("action_root_still_live".into());
    }
    let mut old = journal
        .composition
        .records()
        .filter(|root| root.admission_id == admission);
    let root = old.next().ok_or("action_root_missing")?.clone();
    if old.next().is_some()
        || root.transition_id != transition
        || root.output != certified
        || !journal.closed_actions.certifies_root(&root, generation)
        || resource_generation::historical_generation(journal, &root).ok() != Some(generation)
    {
        return Err("action_root_provenance_invalid".into());
    }
    let mut replacements = journal.composition.records().filter(|item| {
        item.output.origin == certified.origin
            && item.output.producer == owner
            && resource_generation::current_root_generation(journal, item).ok() == Some(current)
    });
    let replacement = replacements.next().ok_or("action_replacement_missing")?;
    if replacements.next().is_some()
        || replacement.output.resource.incarnation == certified.resource.incarnation
    {
        return Err("action_replacement_invalid".into());
    }
    if root
        .plan
        .obligations
        .iter()
        .any(|obligation| session.obligations.contains(obligation))
        || journal.authority.admissions.contains_key(admission)
        || journal
            .authority
            .domain_locks
            .values()
            .any(|id| id == admission)
        || journal
            .receipts
            .values()
            .filter_map(|receipt| receipt.work.as_ref())
            .any(|work| {
                serde_json::to_value(work).map_or(true, |value| contains_id(&value, admission))
            })
        || serde_json::to_value(&journal.authority.control_permits)
            .map_or(true, |v| contains_id(&v, admission))
        || serde_json::to_value(&journal.authority.finalize_permits)
            .map_or(true, |v| contains_id(&v, admission))
    {
        return Err("action_root_referenced".into());
    }
    drop(old);
    drop(replacements);
    let mut next = journal.clone();
    session_prepare_evidence::retire_action_prepare(&mut next, &root)?;
    next.composition
        .retire_action_root_exact(&root)
        .map_err(str::to_owned)?;
    if next.references_admission(admission) {
        return Err("action_root_referenced".into());
    }
    next.authority
        .settled
        .remove(admission)
        .ok_or("action_admission_missing")?;
    next.authority.output_admissions.remove(admission);
    if next.admission_transitions.remove(admission).as_deref() != Some(transition) {
        return Err("action_transition_mismatch".into());
    }
    let session = next
        .sessions
        .get_mut(session_id)
        .ok_or("action_session_missing")?;
    if !session.admission_ids.remove(admission)
        || session.admission_targets.remove(admission).as_ref() != Some(&certified.origin)
        || session.admission_phases.remove(admission) != Some(LifecyclePhase::SessionPrepare)
    {
        return Err("action_admission_scope_mismatch".into());
    }
    validate_journal(&next).map_err(|_| "action_root_retirement_invalid".to_owned())?;
    *journal = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_certificate_leaves_journal_unchanged() {
        let mut journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        let before = serde_json::to_value(&journal).unwrap();
        assert!(retire_one(&mut journal, "old", "transition", "owner", 1).is_err());
        assert_eq!(serde_json::to_value(&journal).unwrap(), before);
        assert!(contains_id(&json!({"nested":[{"admission":"old"}]}), "old"));
        assert!(!contains_id(
            &json!({"nested":[{"admission":"old-other"}]}),
            "old"
        ));
    }
}
