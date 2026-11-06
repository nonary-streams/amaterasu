use super::*;
use amaterasu_module_sdk::display::{ScopedOperation, ScopedStatus, scoped_status_from_reply};

const IDENTITY: &str = "00000000-0000-0000-0000-000000000001";
const REFERENCE: &str = "monitor-retained";

fn status_request(identity: &str, reference_id: &str) -> Value {
    ScopedOperation::ScopedStatus {
        identity: identity.into(),
        reference_id: reference_id.into(),
    }
    .payload()
    .unwrap()
}

fn status(state: &mut State, pipeline: &mut Pipeline<FakePlatform>, caller: &str) -> ScopedStatus {
    scoped_status_from_reply(
        state.run(pipeline, caller, status_request(IDENTITY, REFERENCE)),
        IDENTITY,
        REFERENCE,
    )
    .unwrap()
}

#[test]
fn status_is_read_only_and_does_not_reveal_foreign_or_unmatched_admission() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    assert_eq!(
        status(&mut state, &mut pipeline, "remote").operation_id,
        None
    );
    let request = add_request(&pipeline, "private-monitor-add");
    assert_eq!(state.run(&mut pipeline, "remote", request)["applied"], true);
    let journal = state.journal.clone();
    let revision = state.revision;
    let effects = pipeline.platform.effects.clone();
    // Lookup needs only the selected state provider, even if broad native
    // mutation is disabled and this backend supports only scoped composition.
    pipeline.allow_mutation = false;
    pipeline.platform.scoped_only = true;
    let own = status(&mut state, &mut pipeline, "remote");
    assert_eq!(own.operation_id.as_deref(), Some("private-monitor-add"));
    assert!(!own.pending);
    for (caller, identity, reference) in [
        ("foreign", IDENTITY, REFERENCE),
        ("remote", "different-identity", REFERENCE),
        ("remote", IDENTITY, "different-reference"),
    ] {
        let response = state.run(&mut pipeline, caller, status_request(identity, reference));
        let absent = scoped_status_from_reply(response.clone(), identity, reference).unwrap();
        assert_eq!(absent.operation_id, None);
        assert!(!absent.pending);
        assert!(!response.to_string().contains("private-monitor-add"));
    }
    assert_eq!(state.journal, journal);
    assert_eq!(state.revision, revision);
    assert_eq!(pipeline.platform.effects, effects);
}

#[test]
fn restarted_consumer_discovers_initial_pending_operation_before_or_after_native_effect() {
    for native_applied in [false, true] {
        let mut state = State::default();
        let mut pipeline = pipeline();
        if native_applied {
            pipeline.platform.apply_failure = true;
        } else {
            state.lose_set_acceptance = true;
        }
        let request = add_request(&pipeline, "original-placement");
        let failed = state.run(&mut pipeline, "remote", request);
        assert_ne!(failed["applied"], true);
        let original_journal = state.journal.clone();
        let original_revision = state.revision;
        let original_effects = pipeline.platform.effects.clone();
        pipeline.platform.apply_failure = false;
        // Rebuild the provider and lose all consumer-side operation metadata.
        // Durable state comes only from the provider's real prior write.
        let mut restarted = Pipeline::new(pipeline.platform, false);
        let recovered = status(&mut state, &mut restarted, "remote");
        assert_eq!(
            recovered.operation_id.as_deref(),
            Some("original-placement")
        );
        assert!(recovered.pending);
        assert_eq!(state.journal, original_journal);
        assert_eq!(state.revision, original_revision);
        assert_eq!(restarted.platform.effects, original_effects);
        let foreign = status(&mut state, &mut restarted, "foreign");
        assert_eq!(foreign.operation_id, None);
        assert!(!foreign.pending);
        let reconciled = state.run(
            &mut restarted,
            "remote",
            reconcile_request(recovered.operation_id.as_deref().unwrap()),
        );
        assert_eq!(
            reconciled["state"],
            if native_applied {
                "succeeded"
            } else {
                "not_applied"
            },
            "{reconciled}"
        );
        assert_eq!(restarted.platform.effects, original_effects);
        assert!(!status(&mut state, &mut restarted, "remote").pending);
    }
}

#[test]
fn status_bounds_scope_identifiers_before_returning_journal_metadata() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    for invalid in [String::new(), "x".repeat(129), "bad identity".into()] {
        let result = state.run(&mut pipeline, "remote", status_request(&invalid, REFERENCE));
        assert_eq!(result["error"]["code"], "invalid_request");
    }
    assert!(state.journal.is_none());
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn second_restart_after_unapplied_settlement_allows_fresh_plan_and_normal_restore() {
    for active_game in [false, true] {
        let mut state = State::default();
        let mut pipeline = pipeline();
        let normal = active_game.then(|| {
            let request = normal_request(&pipeline, 75);
            let result = state.run(&mut pipeline, "game", request);
            assert_eq!(result["applied"], true);
            result
        });
        state.lose_set_acceptance = true;
        let request = add_request(&pipeline, "original-placement");
        assert_ne!(state.run(&mut pipeline, "remote", request)["applied"], true);
        let mut restarted = Pipeline::new(pipeline.platform, true);
        assert!(status(&mut state, &mut restarted, "remote").pending);
        let settled = state.run(
            &mut restarted,
            "remote",
            reconcile_request("original-placement"),
        );
        assert_eq!(settled["state"], "not_applied", "{settled}");
        assert_eq!(settled["reference_retained"], true);
        let journal: Journal = serde_json::from_value(state.journal.clone().unwrap()).unwrap();
        assert!(!journal.scoped_targets[0].operation_applied);
        assert!(!journal.scoped_targets[0].primary);
        assert!(journal.scoped_pending.is_none());

        // A second crash loses the not_applied reply. The durable outcome still
        // distinguishes reference retention from a completed native placement.
        let mut restarted_again = Pipeline::new(restarted.platform, true);
        let recovered = status(&mut state, &mut restarted_again, "remote");
        assert_eq!(recovered.operation_id, None);
        assert!(!recovered.pending);
        let stale = state.run(
            &mut restarted_again,
            "remote",
            reconcile_request("original-placement"),
        );
        assert_eq!(stale["error"]["code"], "scoped_operation_not_found");
        if let Some(normal) = normal {
            let restored = state.run(
                &mut restarted_again,
                "game",
                json!({"action":"restore","lease":normal["lease"]}),
            );
            assert_eq!(restored["restored"], true, "{restored}");
            assert_eq!(
                restarted_again
                    .platform
                    .current_snapshot
                    .as_ref()
                    .unwrap()
                    .paths
                    .len(),
                1
            );
        }
        // A normal game may preserve the current desktop while the owner's
        // inactive source reference is waiting for a newly admitted placement.
        let request = normal_request(&restarted_again, 75);
        assert_eq!(
            state.run(&mut restarted_again, "game", request)["applied"],
            true
        );
        let request = add_request(&restarted_again, "fresh-placement");
        assert_eq!(
            state.run(&mut restarted_again, "remote", request)["applied"],
            true
        );
        let completed = status(&mut state, &mut restarted_again, "remote");
        assert_eq!(completed.operation_id.as_deref(), Some("fresh-placement"));
        assert!(!completed.pending);
    }
}

#[test]
fn ambiguous_partial_effect_remains_pending_and_fences_normal_restore() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let request = normal_request(&pipeline, 75);
    let normal = state.run(&mut pipeline, "game", request);
    pipeline.platform.apply_failure = true;
    let request = add_request(&pipeline, "uncertain-placement");
    assert_eq!(
        state.run(&mut pipeline, "remote", request)["scoped_pending"],
        true
    );
    // A native partial result matches neither the admitted before nor after
    // configuration. This changes the fake platform, never the durable journal.
    let target = pipeline
        .platform
        .current_snapshot
        .as_mut()
        .unwrap()
        .paths
        .iter_mut()
        .find(|target| target.id == "monitor.remote")
        .unwrap();
    target.source.width = 1000;
    target.signal.active_width = 1000;
    let mut restarted = Pipeline::new(pipeline.platform, true);
    let before = state.journal.clone();
    let response = state.run(
        &mut restarted,
        "remote",
        reconcile_request("uncertain-placement"),
    );
    assert_eq!(response["code"], "scoped_readback_uncertain", "{response}");
    assert_eq!(state.journal, before);
    let recovered = status(&mut state, &mut restarted, "remote");
    assert_eq!(
        recovered.operation_id.as_deref(),
        Some("uncertain-placement")
    );
    assert!(recovered.pending);
    let restored = state.run(
        &mut restarted,
        "game",
        json!({"action":"restore","lease":normal["lease"]}),
    );
    assert_eq!(restored["error"]["code"], "scoped_recovery_required");
    assert_eq!(state.journal, before);
}

#[test]
fn legacy_completed_reservation_defaults_to_applied_outcome() {
    let mut state = State::default();
    let mut pipeline = pipeline();
    let request = add_request(&pipeline, "legacy-placement");
    assert_eq!(state.run(&mut pipeline, "remote", request)["applied"], true);
    // Decode an older schema fixture without changing the running provider's
    // state or manufacturing any cleanup or completion transition.
    let mut legacy = state.journal.clone().unwrap();
    legacy["scoped_targets"][0]
        .as_object_mut()
        .unwrap()
        .remove("operation_applied");
    let journal: Journal = serde_json::from_value(legacy).unwrap();
    journal.validate().unwrap();
    assert!(journal.scoped_targets[0].operation_applied);
}

#[test]
fn cancelled_reconciliation_does_not_retire_pending_admission() {
    let mut state = State {
        lose_set_acceptance: true,
        ..Default::default()
    };
    let mut pipeline = pipeline();
    let request = add_request(&pipeline, "cancelled-settlement");
    assert_ne!(state.run(&mut pipeline, "remote", request)["applied"], true);
    let journal = state.journal.clone();
    let revision = state.revision;
    state.deny_guard = true;
    let cancelled = state.run(
        &mut pipeline,
        "remote",
        reconcile_request("cancelled-settlement"),
    );
    assert_eq!(cancelled["error"]["code"], "cancelled");
    assert_eq!(state.journal, journal);
    assert_eq!(state.revision, revision);
    let pending = status(&mut state, &mut pipeline, "remote");
    assert_eq!(
        pending.operation_id.as_deref(),
        Some("cancelled-settlement")
    );
    assert!(pending.pending);
    assert!(pipeline.platform.effects.is_empty());
}

#[test]
fn pending_journal_identity_must_match_its_exact_owned_reference() {
    let mut state = State {
        lose_set_acceptance: true,
        ..Default::default()
    };
    let mut pipeline = pipeline();
    let request = add_request(&pipeline, "owned-admission");
    assert_ne!(state.run(&mut pipeline, "remote", request)["applied"], true);
    let valid: Journal = serde_json::from_value(state.journal.clone().unwrap()).unwrap();
    valid.validate().unwrap();
    for field in ["identity", "reference_id"] {
        let mut malformed = valid.clone();
        malformed.scoped_pending.as_mut().unwrap().request_identity[field] = json!("foreign");
        assert_eq!(malformed.validate().unwrap_err().code, "invalid_journal");
    }
}
