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
        limiter_executable_path: None,
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
fn restart_restores_only_the_journaled_limiter_executable() {
    const RECORD_ID: u64 = 0xabcd;
    const EXECUTABLE: &str = r"C:\Games\Example.exe";
    let mut document: Document =
        serde_json::from_value(journal_with_owned_session(RECORD_ID)).unwrap();
    let entry = document.entries.get_mut(&RECORD_ID).unwrap();
    entry.attempted_resources.clear();
    entry.limiter_attempted = true;
    entry.limiter_executable_path = Some(EXECUTABLE.into());
    let mut host = Harness::with_restart_state(serde_json::to_value(document).unwrap(), 7);
    host.broker.limiter_session = Some(format!("stream-{RECORD_ID:016x}"));
    host.broker.limiter_lease = Some("owned-limiter-lease".into());
    host.broker.limiter_executable_path = Some(EXECUTABLE.into());

    host.idle();

    assert!(host.broker.limiter_lease.is_none());
    assert_eq!(host.broker.state_revision, 8);
    assert_eq!(
        host.broker.state_value.as_ref().unwrap()["entries"],
        json!({})
    );
    assert!(host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.frame-limiter/1"
            && call.payload["action"] == "restore"
            && call.payload["lease"] == "owned-limiter-lease"
    }));
    assert_eq!(host.response(1, prepare())["state"], "prepared");
}

#[test]
fn legacy_limiter_intent_without_identity_blocks_recovery() {
    let mut document = journal_with_owned_session(0xabcd);
    document["entries"]["43981"]["attempted_resources"] = json!([]);
    document["entries"]["43981"]["limiter_attempted"] = json!(true);
    let mut host = Harness::with_restart_state(document, 7);
    host.broker.limiter_session = Some("stream-000000000000abcd".into());
    host.broker.limiter_lease = Some("unverified-limiter".into());

    host.idle();

    assert_eq!(host.broker.state_revision, 7);
    assert_eq!(
        host.broker.limiter_lease.as_deref(),
        Some("unverified-limiter")
    );
    assert_eq!(
        host.response(1, prepare())["code"],
        "restart_recovery_unconfirmed"
    );
    assert!(!host.broker.calls.iter().any(|call| {
        call.service == "amaterasu.frame-limiter/1" && call.payload["action"] == "restore"
    }));
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
