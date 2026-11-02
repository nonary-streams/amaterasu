use super::*;
use crate::pipeline::journal::{Document, Entry};
use std::collections::BTreeSet;

fn journal_with_owned_session(record_id: u64) -> Value {
    let entry = Entry {
        providers: Default::default(),
        lifecycle_recovery: None,
        record_id,
        owner: "owner".into(),
        attempted_resources: BTreeSet::from(["session".to_owned()]),
        input_attempted: false,
        virtual_display_attempted: false,
        virtual_display: None,
        display_attempted: false,
        display_operation_id: None,
        legacy_effect_pending: false,
        legacy_effect_target: None,
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
fn legacy_effect_intent_is_retained_and_never_reported_as_clean() {
    let mut document = journal_with_owned_session(0xabcd);
    document["entries"]["43981"]["attempted_resources"] = json!([]);
    document["entries"]["43981"]["limiter_attempted"] = json!(true);
    document["entries"]["43981"]["limiter_executable_path"] = json!(r"C:\Games\Example.exe");
    let original = document.clone();
    let mut host = Harness::with_restart_state(document, 7);
    host.idle();
    assert_eq!(host.broker.state_revision, 7);
    assert_eq!(host.broker.state_value, Some(original));
    assert_eq!(
        host.response(1, prepare())["code"],
        "restart_recovery_unconfirmed"
    );
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.frame-limiter/1")
    );
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

fn journal_with_virtual_display(record_id: u64) -> Value {
    let mut document = journal_with_owned_session(record_id);
    let entry = &mut document["entries"][record_id.to_string()];
    entry["attempted_resources"] = json!([]);
    entry["virtual_display_attempted"] = json!(true);
    entry["virtual_display"] = json!({
        "identity":"00000000-0000-0000-0000-000000000042",
        "reference_id":"original-stream-reference",
        "name":"Shared monitor",
        "width":1920,
        "height":1080,
        "refresh_millihz":60000
    });
    document
}

#[test]
fn restart_releases_only_original_virtual_display_reference_after_display_restore() {
    const RECORD_ID: u64 = 0xabcd;
    let mut document = journal_with_virtual_display(RECORD_ID);
    // Restore ownership may have transferred from another stream before restart.
    document["entries"]["43981"]["display_attempted"] = json!(true);
    document["entries"]["43981"]["display_operation_id"] = json!("ss-1234-display");
    let mut host = Harness::with_restart_state(document.clone(), 7);
    let mut specification = document["entries"]["43981"]["virtual_display"].clone();
    specification
        .as_object_mut()
        .unwrap()
        .remove("reference_id");
    host.broker.virtual_display_specification = Some(specification.clone());
    host.broker.virtual_display_references = BTreeSet::from([
        "original-stream-reference".into(),
        "unrelated-shared-reference".into(),
    ]);
    host.broker.display_operation = Some("ss-1234-display".into());
    host.broker.display_lease = Some("display-0000000000000001".into());

    host.idle();

    assert!(host.broker.display_lease.is_none());
    assert_eq!(
        host.broker.virtual_display_specification,
        Some(specification)
    );
    assert_eq!(
        host.broker.virtual_display_references,
        BTreeSet::from(["unrelated-shared-reference".into()])
    );
    assert_eq!(
        host.broker.state_value.as_ref().unwrap()["entries"],
        json!({})
    );
    let restore = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.display/1" && call.payload["action"] == "restore"
        })
        .unwrap();
    let remove = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "remove"
        })
        .unwrap();
    assert!(restore < remove);
    let removal = &host.broker.calls[remove].payload;
    assert_eq!(removal["identity"], "00000000-0000-0000-0000-000000000042");
    assert_eq!(removal["reference_id"], "original-stream-reference");
}

#[test]
fn invalid_restart_virtual_display_identity_keeps_journal_without_provider_mutation() {
    let mut document = journal_with_virtual_display(0xabcd);
    document["entries"]["43981"]["virtual_display"]["reference_id"] = json!("bad\nreference");
    let mut host = Harness::with_restart_state(document, 7);

    host.idle();

    assert_eq!(host.broker.state_revision, 7);
    assert_eq!(
        host.response(1, prepare())["code"],
        "restart_recovery_unconfirmed"
    );
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.virtual-display/1")
    );
}

#[test]
fn restart_keeps_virtual_display_reference_when_topology_restore_is_unconfirmed() {
    let mut document = journal_with_virtual_display(0xabcd);
    document["entries"]["43981"]["display_attempted"] = json!(true);
    document["entries"]["43981"]["display_operation_id"] = json!("ss-1234-display");
    let mut host = Harness::with_restart_state(document, 7);
    host.broker.display_operation = Some("ss-foreign-display".into());
    host.broker.display_lease = Some("display-0000000000000001".into());
    host.broker
        .virtual_display_references
        .insert("original-stream-reference".into());

    host.idle();

    assert!(
        host.broker
            .virtual_display_references
            .contains("original-stream-reference")
    );
    assert_eq!(host.broker.state_revision, 7);
    assert_eq!(
        host.response(1, prepare())["code"],
        "restart_recovery_unconfirmed"
    );
    assert!(!host.broker.calls.iter().any(|call| {
        (call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "remove")
            || (call.service == "amaterasu.display/1" && call.payload["action"] == "restore")
    }));
}
