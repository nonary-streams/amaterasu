use super::*;
use serde_json::json;

fn entry() -> Entry {
    Entry {
        id: "opaque/game".into(),
        revision: "row-v1".into(),
        name: "Game".into(),
        description: None,
        tags: vec![],
        source: None,
        portable_ids: vec![],
        installed: Installed::Unknown,
        readiness: Readiness::Unknown,
        unavailable_reason: None,
    }
}

#[test]
fn independent_revisions_and_opaque_ids_are_allowed_but_invalid_pages_are_not() {
    let mut page = Page {
        revision: "snapshot-v3".into(),
        entries: vec![entry()],
        next_cursor: None,
    };
    assert!(page.validate_for(None, 1).is_ok());
    page.next_cursor = Some(Cursor {
        revision: "snapshot-v3".into(),
        token: "next".into(),
    });
    let cursor = page.next_cursor.clone().unwrap();
    assert!(page.validate_for(Some(&cursor), 1).is_err());
    page.next_cursor.as_mut().unwrap().token = "advance".into();
    assert!(page.validate_for(Some(&cursor), 1).is_ok());
    page.revision = "new-snapshot".into();
    assert!(page.validate_for(Some(&cursor), 1).is_err());
    page.revision = "snapshot-v3".into();
    page.entries.clear();
    assert!(page.validate_for(None, 1).is_err());
    page.entries = vec![entry(), entry()];
    assert!(page.validate_for(None, 2).is_err());
}

#[test]
fn metadata_projection_rejects_private_fields_and_preserves_unknown_facts() {
    let encoded = serde_json::to_value(entry()).unwrap();
    assert_eq!(encoded["installed"], "unknown");
    for field in ["command", "path", "credentials", "service"] {
        let mut private = encoded.clone();
        private[field] = json!("private");
        assert!(serde_json::from_value::<Entry>(private).is_err());
    }
    let mut oversized = entry();
    oversized.description = Some("x".repeat(2049));
    assert!(oversized.validate().is_err());
}

#[test]
fn version_selection_and_handoff_are_exact() {
    assert!(
        Capabilities {
            version: 3,
            features: [RESOLVE_LAUNCH.into()].into()
        }
        .validate()
        .is_err()
    );
    let target = LaunchTarget {
        id: "opaque/game".into(),
        revision: "row-v1".into(),
        executor: "runner".into(),
        application_id: "42".into(),
    };
    assert!(target.validate_for("opaque/game", "row-v1").is_ok());
    assert!(target.validate_for("other", "row-v1").is_err());
    assert!(target.validate_for("opaque/game", "changed").is_err());
    let mut injected = serde_json::to_value(target).unwrap();
    injected["service"] = json!("unapproved/1");
    assert!(serde_json::from_value::<LaunchTarget>(injected).is_err());
    for field in ["caller", "initiator", "context", "certificate_id"] {
        let mut request = json!({"action":"resolve_launch","id":"opaque/game","revision":"row-v1"});
        request[field] = json!("forged");
        assert!(serde_json::from_value::<Request>(request).is_err());
    }
}

#[test]
fn optional_capabilities_can_grow_without_becoming_route_authority() {
    let mut capabilities = Capabilities {
        version: 2,
        features: [SEARCH.into(), "future.library-feature/7".into()].into(),
    };
    assert!(capabilities.validate().is_ok());
    assert!(capabilities.supports(SEARCH));
    assert!(!capabilities.supports(RESOLVE_LAUNCH));
    let roundtrip: Capabilities =
        serde_json::from_value(serde_json::to_value(&capabilities).unwrap()).unwrap();
    assert_eq!(roundtrip, capabilities);
    for invalid in [
        "unversioned",
        "feature/0",
        "feature/01",
        "feature/999999999999",
        "../feature/1",
    ] {
        capabilities.features = [invalid.into()].into();
        assert!(capabilities.validate().is_err(), "{invalid}");
    }
}
