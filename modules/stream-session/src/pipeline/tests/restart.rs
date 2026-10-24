use super::*;
use crate::pipeline::journal::{Document, Entry};
use std::collections::BTreeSet;

fn journal_with_owned_session(record_id: u64) -> Value {
    let entry = Entry {
        record_id,
        owner: "owner".into(),
        attempted_resources: BTreeSet::from(["session".to_owned()]),
        input_attempted: false,
        display_attempted: false,
        limiter_attempted: false,
    };
    let mut document = Document::default();
    document.entries.insert(record_id, entry);
    serde_json::to_value(document).unwrap()
}

#[test]
fn restart_reconciles_only_exact_journaled_session_then_allows_relaunch() {
    const RECORD_ID: u64 = 0xabcd;
    let mut host = Harness::with_restart_state(journal_with_owned_session(RECORD_ID), 7);
    host.broker.seed_session("ss-abcd-session");
    host.broker.seed_session("unrelated-session-key");

    host.idle();

    assert!(
        host.broker
            .session_is_stopped_without_leases("ss-abcd-session")
    );
    assert!(host.broker.has_session_key("unrelated-session-key"));
    assert_eq!(host.broker.state_revision, 8);
    assert_eq!(
        host.broker.state_value.as_ref().unwrap()["entries"],
        json!({})
    );
    assert!(host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.session/1"
            && call.payload["action"] == "lookup"
            && call.payload["key"] == "ss-abcd-session"
    }));

    let prepared = host.response(1, prepare());
    assert_eq!(prepared["state"], "prepared", "{prepared}");
}

#[test]
fn uncertain_restart_cleanup_keeps_journal_and_blocks_new_sessions() {
    const RECORD_ID: u64 = 0xabcd;
    let mut host = Harness::with_restart_state(journal_with_owned_session(RECORD_ID), 7);
    host.broker.seed_session("ss-abcd-session");
    host.broker.fail_session_stop = true;

    host.idle();

    assert_eq!(host.broker.state_revision, 7);
    assert_eq!(
        host.broker.state_value.as_ref().unwrap()["entries"]["43981"]["record_id"],
        RECORD_ID
    );
    let denied = host.response(1, prepare());
    assert_eq!(denied["code"], "restart_recovery_unconfirmed");
    assert!(!host.broker.calls.iter().any(|call| {
        call.payload["action"] == "create" && call.service == "amaterasu.session/1"
    }));
}

#[test]
fn failed_intent_cas_never_reaches_a_provider_open() {
    let mut host =
        Harness::with_restart_state(serde_json::to_value(Document::default()).unwrap(), 0);
    host.broker.state_fail_set = true;

    let response = host.response(1, prepare());

    assert_ne!(response["state"], "prepared");
    assert!(!host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.session/1" && call.payload["action"] == "create"
    }));
    assert_eq!(host.broker.state_revision, 0);
    assert_eq!(
        host.response(2, prepare())["code"],
        "restart_recovery_unconfirmed"
    );
}
