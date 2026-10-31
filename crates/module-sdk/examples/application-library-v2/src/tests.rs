use super::*;
use amaterasu_module_sdk::application_library::v2::{Installed, SourceIdentity};
use amaterasu_module_sdk::requests::IncomingRequest;
use serde_json::{Value, json};

fn entry(id: &str) -> Entry {
    Entry {
        id: id.into(),
        revision: "entry-v1".into(),
        name: format!("Game {id}"),
        description: None,
        tags: vec![],
        source: Some(SourceIdentity {
            provider: "store".into(),
            id: "shared-source-id".into(),
        }),
        portable_ids: vec![],
        installed: Installed::Unknown,
        readiness: Readiness::Ready,
        unavailable_reason: None,
    }
}

fn adapter() -> LibraryAdapter<SnapshotLibrary> {
    let entries = vec![entry("opaque/a"), entry("opaque/b"), entry("opaque/c")];
    let target = LaunchTarget {
        id: "opaque/a".into(),
        revision: "entry-v1".into(),
        executor: "runner".into(),
        application_id: "runner-specific-id".into(),
    };
    let library = SnapshotLibrary::new("snapshot".into(), entries, vec![target]).unwrap();
    LibraryAdapter::new(
        library,
        "example.library/2".into(),
        BTreeMap::from([
            (
                "reader".into(),
                CallerPolicy {
                    initiators: ["reader".into()].into(),
                    read: true,
                    resolve_launch: false,
                },
            ),
            (
                "launcher".into(),
                CallerPolicy {
                    initiators: ["reader".into()].into(),
                    read: false,
                    resolve_launch: true,
                },
            ),
        ]),
    )
    .unwrap()
}

fn request(caller: &str, initiator: &str, payload: Value) -> IncomingRequest<Value> {
    IncomingRequest {
        id: 1,
        caller: caller.into(),
        initiator: initiator.into(),
        service: "example.library/2".into(),
        payload,
        remaining_ms: 1000,
    }
}

#[test]
fn paginate_without_numeric_ids_and_reject_changed_scope_query_revision() {
    let mut library = adapter();
    let list = |cursor| json!({"action":"list","query":{"text":"Game"},"cursor":cursor,"limit":1});
    let first = library.dispatch(&request("reader", "reader", list(Value::Null)));
    assert_eq!(first["page"]["entries"][0]["id"], "opaque/a");
    assert_eq!(first["page"]["entries"][0]["installed"], "unknown");
    let cursor = first["page"]["next_cursor"].clone();
    let next = library.dispatch(&request("reader", "reader", list(cursor.clone())));
    assert_eq!(next["page"]["entries"][0]["id"], "opaque/b");
    let last = library.dispatch(&request(
        "reader",
        "reader",
        list(next["page"]["next_cursor"].clone()),
    ));
    assert_eq!(last["page"]["entries"][0]["id"], "opaque/c");
    assert!(last["page"]["next_cursor"].is_null());
    let mut changed = list(cursor.clone());
    changed["query"]["text"] = json!("other");
    assert_eq!(
        library.dispatch(&request("reader", "reader", changed))["error"]["code"],
        "invalid_cursor"
    );
    let mut stale = cursor;
    stale["revision"] = json!("old");
    assert_eq!(
        library.dispatch(&request("reader", "reader", list(stale)))["error"]["code"],
        "stale_cursor"
    );
    assert_eq!(
        library.dispatch(&request("reader", "forged", list(Value::Null)))["error"]["code"],
        "unauthorized"
    );
}

#[test]
fn launch_resolution_requires_permission_and_exact_revision_and_readiness() {
    let mut library = adapter();
    let target = json!({"action":"resolve_launch","id":"opaque/a","revision":"entry-v1"});
    assert_eq!(
        library.dispatch(&request("reader", "reader", target.clone()))["error"]["code"],
        "unauthorized"
    );
    assert_eq!(
        library.dispatch(&request("launcher", "reader", target.clone()))["target"]["application_id"],
        "runner-specific-id"
    );
    let mut stale = target.clone();
    stale["revision"] = json!("old");
    assert_eq!(
        library.dispatch(&request("launcher", "reader", stale))["error"]["code"],
        "revision_conflict"
    );
    for field in ["caller", "service", "command", "context"] {
        let mut forged = target.clone();
        forged[field] = json!("injected");
        assert_eq!(
            library.dispatch(&request("launcher", "reader", forged))["error"]["code"],
            "invalid_request"
        );
    }
    let mut missing = entry("uninstalled");
    missing.readiness = Readiness::Unavailable;
    let mut snapshot = SnapshotLibrary::new("one".into(), vec![missing], vec![]).unwrap();
    assert_eq!(
        snapshot.resolve_launch(
            &Identity {
                caller: "launcher",
                initiator: "reader"
            },
            "uninstalled",
            "entry-v1"
        ),
        Err("not_ready")
    );
}

#[test]
fn snapshot_rejects_duplicate_library_ids_and_keeps_source_ids_separate() {
    assert!(SnapshotLibrary::new("v1".into(), vec![entry("a"), entry("a")], vec![]).is_err());
    assert!(SnapshotLibrary::new("v1".into(), vec![entry("a"), entry("b")], vec![]).is_ok());
}
