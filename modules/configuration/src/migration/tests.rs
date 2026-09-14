use super::*;
use crate::protocol::{Change, Operation, apply};

fn document() -> Document {
    let operation = serde_json::from_value(json!({"action":"register","fields":{
        "enabled":{"type":"boolean","default":true},
        "title":{"type":"string","default":"custom title"},
        "count":{"type":"integer","default":7,"minimum":1,"maximum":10}
    }}))
    .unwrap();
    let Change::Write(document) = apply(operation, None).unwrap() else {
        panic!()
    };
    document
}
fn migration(payload: Value, document: Document) -> Result<Document, Value> {
    let operation: Operation = serde_json::from_value(payload).unwrap();
    match apply(operation, Some(document))? {
        Change::Write(document) => Ok(document),
        Change::Read(_) => panic!("migration must commit a revision"),
    }
}
fn replacement() -> Value {
    serde_json::to_value(document().fields).unwrap()
}

#[test]
fn rename_preserves_custom_values_and_new_fields_receive_validated_defaults() {
    let original = document();
    let changed = migration(
        json!({"action":"migrate","expected_revision":1,"fields":{
        "active":{"type":"boolean","default":false},
        "title":{"type":"string","default":"new default"},
        "count":{"type":"integer","default":1},
        "new_field":{"type":"string","default":"added"}
    },"renames":{"enabled":"active"}}),
        original,
    )
    .unwrap();
    assert_eq!(changed.revision, 2);
    assert_eq!(changed.values["active"], true);
    assert_eq!(changed.values["title"], "custom title");
    assert_eq!(changed.values["count"], 7);
    assert_eq!(changed.values["new_field"], "added");
    assert!(!changed.values.contains_key("enabled"));
    let reloaded: Document =
        serde_json::from_slice(&serde_json::to_vec(&changed).unwrap()).unwrap();
    assert_eq!(reloaded.values, changed.values);
}

#[test]
fn removed_fields_are_explicit_and_can_reset_retained_names() {
    let fields = json!({"title":{"type":"string","default":"reset"}});
    let failure = migration(
        json!({"action":"migrate","expected_revision":1,"fields":fields}),
        document(),
    )
    .err()
    .unwrap();
    assert_eq!(failure["error"]["code"], "invalid_migration");
    let changed = migration(json!({"action":"migrate","expected_revision":1,"fields":fields,"removed_fields":["enabled","count","title"]}), document()).unwrap();
    assert_eq!(
        changed.values,
        Values::from([("title".into(), json!("reset"))])
    );
    for removed in [json!(["missing"]), json!(["enabled", "enabled"])] {
        assert!(migration(json!({"action":"migrate","expected_revision":1,"fields":replacement(),"removed_fields":removed}), document()).is_err());
    }
}

#[test]
fn incompatible_types_and_tighter_bounds_require_valid_explicit_replacements() {
    let mut fields = replacement();
    fields["count"] = json!({"type":"string","default":"zero"});
    let request = json!({"action":"migrate","expected_revision":1,"fields":fields});
    assert_eq!(
        migration(request.clone(), document()).err().unwrap()["error"]["code"],
        "invalid_values"
    );
    let mut explicit = request;
    explicit["values"] = json!({"count":"seven"});
    assert_eq!(
        migration(explicit, document()).unwrap().values["count"],
        "seven"
    );
    let mut fields = replacement();
    fields["count"] = json!({"type":"integer","default":1,"maximum":5});
    let mut request = json!({"action":"migrate","expected_revision":1,"fields":fields});
    assert_eq!(
        migration(request.clone(), document()).err().unwrap()["error"]["code"],
        "invalid_values"
    );
    request["values"] = json!({"count":4});
    assert_eq!(migration(request, document()).unwrap().values["count"], 4);
}

#[test]
fn mapping_collisions_are_rejected_and_renames_are_simultaneous() {
    for renames in [
        json!({"count":"title"}),
        json!({"enabled":"missing"}),
        json!({"missing":"title"}),
    ] {
        assert_eq!(migration(json!({"action":"migrate","expected_revision":1,"fields":replacement(),"renames":renames}), document()).err().unwrap()["error"]["code"], "invalid_migration");
    }
    let fields = json!({"enabled":{"type":"boolean","default":false},"count":{"type":"string","default":"default"},"title":{"type":"integer","default":0}});
    let swapped = migration(json!({"action":"migrate","expected_revision":1,"fields":fields,"renames":{"count":"title","title":"count"}}), document()).unwrap();
    assert_eq!(swapped.values["title"], 7);
    assert_eq!(swapped.values["count"], "custom title");
    assert!(migration(json!({"action":"migrate","expected_revision":1,"fields":replacement(),"renames":{"title":"title"},"removed_fields":["title"]}), document()).is_err());
}

#[test]
fn stale_revisions_invalid_defaults_and_authority_overrides_are_rejected() {
    let failure = migration(
        json!({"action":"migrate","expected_revision":0,"fields":replacement()}),
        document(),
    )
    .err()
    .unwrap();
    assert_eq!(failure["error"]["current_revision"], 1);
    let mut fields = replacement();
    fields["count"] = json!({"type":"integer","default":5,"maximum":2});
    assert_eq!(
        migration(
            json!({"action":"migrate","expected_revision":1,"fields":fields}),
            document()
        )
        .err()
        .unwrap()["error"]["code"],
        "invalid_schema"
    );
    for invalid in [
        json!({"action":"migrate","fields":replacement()}),
        json!({"action":"migrate","expected_revision":1,"fields":replacement(),"owner":"other"}),
        json!({"action":"migrate","expected_revision":1,"fields":replacement(),"script":"run something"}),
    ] {
        assert!(serde_json::from_value::<Operation>(invalid).is_err());
    }
}
