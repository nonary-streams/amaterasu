use super::*;
use serde_json::Value;

fn create(coordinator: &mut Coordinator, caller: &str, key: &str) -> u64 {
    let response = coordinator.handle(caller, json!({"action":"create","key":key}));
    assert_eq!(response["ok"], true, "{response}");
    response["sessions"][0]["id"].as_u64().unwrap()
}

fn action(coordinator: &mut Coordinator, caller: &str, id: u64, action: &str) -> Value {
    coordinator.handle(caller, json!({"action":action,"session_id":id}))
}

fn acquire(
    coordinator: &mut Coordinator,
    caller: &str,
    id: u64,
    resource: &str,
    mode: &str,
) -> Value {
    coordinator.handle(
        caller,
        json!({"action":"acquire","session_id":id,"resource":resource,"mode":mode}),
    )
}

fn release(coordinator: &mut Coordinator, caller: &str, id: u64, resource: &str) -> Value {
    coordinator.handle(
        caller,
        json!({"action":"release","session_id":id,"resource":resource}),
    )
}

#[test]
fn callers_cannot_inspect_mutate_or_forget_another_session() {
    let mut coordinator = Coordinator::default();
    let alice = create(&mut coordinator, "alice", "same-key");
    let bob = create(&mut coordinator, "bob", "same-key");
    assert_ne!(alice, bob);
    for operation in [
        "snapshot",
        "activate",
        "stop",
        "finish_stop",
        "cancel",
        "fail",
        "forget",
    ] {
        assert_eq!(
            action(&mut coordinator, "bob", alice, operation)["code"],
            "session_not_found"
        );
    }
    assert_eq!(
        acquire(&mut coordinator, "bob", alice, "screen", "exclusive")["code"],
        "session_not_found"
    );
    assert_eq!(
        release(&mut coordinator, "bob", alice, "screen")["code"],
        "session_not_found"
    );
    let list = coordinator.handle("bob", json!({"action":"list"}));
    assert_eq!(list["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(list["sessions"][0]["id"], bob);
}

#[test]
fn retries_do_not_duplicate_sessions_leases_or_transitions() {
    let mut coordinator = Coordinator::default();
    let id = create(&mut coordinator, "alice", "request-1");
    assert_eq!(create(&mut coordinator, "alice", "request-1"), id);
    assert_eq!(coordinator.revision(), 1);
    assert_eq!(
        acquire(&mut coordinator, "alice", id, "screen", "shared")["ok"],
        true
    );
    let revision = coordinator.revision();
    acquire(&mut coordinator, "alice", id, "screen", "shared");
    assert_eq!(coordinator.revision(), revision);
    action(&mut coordinator, "alice", id, "activate");
    let revision = coordinator.revision();
    action(&mut coordinator, "alice", id, "activate");
    assert_eq!(coordinator.revision(), revision);
    action(&mut coordinator, "alice", id, "stop");
    let revision = coordinator.revision();
    action(&mut coordinator, "alice", id, "stop");
    assert_eq!(coordinator.revision(), revision);
    let stopped = action(&mut coordinator, "alice", id, "finish_stop");
    assert_eq!(stopped["sessions"][0]["state"], "stopped");
    assert_eq!(stopped["sessions"][0]["leases"], json!({}));
    let revision = coordinator.revision();
    for operation in ["stop", "finish_stop", "cancel", "fail"] {
        assert_eq!(
            action(&mut coordinator, "alice", id, operation)["sessions"][0]["outcome"],
            "completed"
        );
    }
    release(&mut coordinator, "alice", id, "screen");
    assert_eq!(coordinator.revision(), revision);
}

#[test]
fn shared_owners_survive_other_owners_release_and_final_owner_removes_resource() {
    let mut coordinator = Coordinator::default();
    let a = create(&mut coordinator, "alice", "a");
    let b = create(&mut coordinator, "bob", "b");
    let c = create(&mut coordinator, "carol", "c");
    assert_eq!(
        acquire(&mut coordinator, "alice", a, "screen", "shared")["ok"],
        true
    );
    assert_eq!(
        acquire(&mut coordinator, "bob", b, "screen", "shared")["ok"],
        true
    );
    assert_eq!(
        acquire(&mut coordinator, "carol", c, "screen", "exclusive")["code"],
        "resource_busy"
    );
    action(&mut coordinator, "alice", a, "cancel");
    release(&mut coordinator, "alice", a, "screen");
    assert_eq!(
        acquire(&mut coordinator, "carol", c, "screen", "exclusive")["code"],
        "resource_busy"
    );
    release(&mut coordinator, "bob", b, "screen");
    let revision = coordinator.revision();
    release(&mut coordinator, "bob", b, "screen");
    assert_eq!(coordinator.revision(), revision);
    assert_eq!(
        acquire(&mut coordinator, "carol", c, "screen", "exclusive")["ok"],
        true
    );
    assert_eq!(
        acquire(&mut coordinator, "bob", b, "screen", "shared")["code"],
        "resource_busy"
    );
    assert_eq!(
        acquire(&mut coordinator, "carol", c, "screen", "shared")["code"],
        "lease_mode_conflict"
    );
}

#[test]
fn failed_partial_start_cleans_only_its_leases_and_snapshots_resync() {
    let mut coordinator = Coordinator::default();
    let a = create(&mut coordinator, "alice", "a");
    let b = create(&mut coordinator, "bob", "b");
    acquire(&mut coordinator, "alice", a, "camera", "exclusive");
    acquire(&mut coordinator, "bob", b, "audio", "exclusive");
    let initial = action(&mut coordinator, "alice", a, "snapshot");
    let failed = action(&mut coordinator, "alice", a, "fail");
    assert_eq!(failed["sessions"][0]["outcome"], "failed");
    assert!(failed["revision"].as_u64().unwrap() > initial["revision"].as_u64().unwrap());
    assert_eq!(action(&mut coordinator, "alice", a, "snapshot"), failed);
    assert_eq!(
        acquire(&mut coordinator, "bob", b, "camera", "exclusive")["ok"],
        true
    );
    let c = create(&mut coordinator, "carol", "c");
    assert_eq!(
        acquire(&mut coordinator, "carol", c, "audio", "shared")["code"],
        "resource_busy"
    );
    assert_eq!(
        action(&mut coordinator, "alice", a, "activate")["code"],
        "invalid_transition"
    );
    assert_eq!(
        acquire(&mut coordinator, "alice", a, "other", "shared")["code"],
        "invalid_transition"
    );
}

#[test]
fn invalid_transitions_and_forgotten_ids_do_not_mutate_state() {
    let mut coordinator = Coordinator::default();
    let id = create(&mut coordinator, "alice", "key");
    for operation in ["finish_stop", "forget"] {
        assert_eq!(
            action(&mut coordinator, "alice", id, operation)["code"],
            "invalid_transition"
        );
    }
    assert_eq!(coordinator.revision(), 1);
    action(&mut coordinator, "alice", id, "cancel");
    action(&mut coordinator, "alice", id, "forget");
    let revision = coordinator.revision();
    for operation in ["snapshot", "activate", "stop", "cancel", "forget"] {
        assert_eq!(
            action(&mut coordinator, "alice", id, operation)["code"],
            "session_not_found"
        );
    }
    assert_eq!(coordinator.revision(), revision);
    assert!(create(&mut coordinator, "alice", "key") > id);
}

#[test]
fn bounded_retention_requires_explicit_forgetting() {
    let mut coordinator = Coordinator::default();
    let first = create(&mut coordinator, "alice", "0");
    for index in 1..256 {
        create(&mut coordinator, "alice", &index.to_string());
    }
    assert_eq!(
        coordinator.handle("alice", json!({"action":"create","key":"overflow"}))["code"],
        "session_limit"
    );
    action(&mut coordinator, "alice", first, "cancel");
    assert_eq!(
        coordinator.handle("alice", json!({"action":"create","key":"overflow"}))["code"],
        "session_limit"
    );
    action(&mut coordinator, "alice", first, "forget");
    create(&mut coordinator, "alice", "overflow");
}

#[test]
fn invalid_requests_and_lease_limits_are_domain_errors() {
    let mut coordinator = Coordinator::default();
    assert_eq!(
        coordinator.handle("alice", json!({"action":"create","key":"x","owner":"bob"}))["code"],
        "invalid_request"
    );
    let id = create(&mut coordinator, "alice", "x");
    assert_eq!(
        acquire(&mut coordinator, "alice", id, "", "shared")["code"],
        "invalid_resource"
    );
    for index in 0..128 {
        assert_eq!(
            acquire(&mut coordinator, "alice", id, &index.to_string(), "shared")["ok"],
            true
        );
    }
    assert_eq!(
        acquire(&mut coordinator, "alice", id, "overflow", "shared")["code"],
        "lease_limit"
    );
    assert_eq!(
        acquire(&mut coordinator, "alice", id, "0", "shared")["ok"],
        true
    );
}

#[test]
fn escaped_keys_and_resources_fit_runtime_payload_limit_with_pagination() {
    const PAYLOAD_LIMIT: usize = 16_384;
    let mut coordinator = Coordinator::default();
    let mut ids = Vec::new();
    for index in 0..33 {
        let key = format!("{}{:03}", "\u{0001}".repeat(125), index);
        ids.push(create(&mut coordinator, "alice", &key));
    }
    create(&mut coordinator, "bob", "hidden");
    let mut collected = Vec::new();
    let mut offset = 0;
    loop {
        let page = coordinator.handle("alice", json!({"action":"list","offset":offset,"limit":16}));
        assert!(serde_json::to_vec(&page).unwrap().len() < PAYLOAD_LIMIT);
        collected.extend(
            page["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|session| session["id"].as_u64().unwrap()),
        );
        match page["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(collected, ids);
    let id = ids[0];
    for index in 0..128 {
        let resource = format!("{}{:03}", "\u{0001}".repeat(9), index);
        let response = acquire(&mut coordinator, "alice", id, &resource, "exclusive");
        assert_eq!(response["ok"], true, "{response}");
        assert!(serde_json::to_vec(&response).unwrap().len() < PAYLOAD_LIMIT);
    }
    let snapshot = action(&mut coordinator, "alice", id, "snapshot");
    assert!(serde_json::to_vec(&snapshot).unwrap().len() < PAYLOAD_LIMIT);
    let other = ids[1];
    for index in 0..6 {
        let resource = format!("{}{}", "\u{0001}".repeat(255), index);
        assert_eq!(
            acquire(&mut coordinator, "alice", other, &resource, "exclusive")["ok"],
            true
        );
    }
    let revision = coordinator.revision();
    assert_eq!(
        acquire(&mut coordinator, "alice", other, "overflow", "shared")["code"],
        "lease_limit"
    );
    assert_eq!(coordinator.revision(), revision);
    let snapshot = action(&mut coordinator, "alice", other, "snapshot");
    assert!(serde_json::to_vec(&snapshot).unwrap().len() < PAYLOAD_LIMIT);
    assert_eq!(
        coordinator.handle("alice", json!({"action":"list","limit":17}))["code"],
        "invalid_limit"
    );
}

#[test]
fn lookup_recovers_only_owned_session_without_creating_or_changing_revision() {
    let mut coordinator = Coordinator::default();
    let id = create(&mut coordinator, "owner", "recoverable-key");
    let revision = coordinator.revision();
    let request = json!({"action":"lookup","key":"recoverable-key"});
    let found = coordinator.handle("owner", request.clone());
    assert_eq!(found["sessions"][0]["id"], id);
    assert_eq!(
        coordinator.handle("foreign", request)["code"],
        "session_not_found"
    );
    assert_eq!(
        coordinator.handle("owner", json!({"action":"lookup","key":"absent"}))["code"],
        "session_not_found"
    );
    assert_eq!(coordinator.revision(), revision);
}
