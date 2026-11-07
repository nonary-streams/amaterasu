//! The complete occupancy projection remains separate from the ordinary list.
use amaterasu_module_sdk::application_library::LibraryIdentityInventoryResponse;
use serde_json::{Value, json};

pub(super) fn project(payload: Value, limit: usize) -> Value {
    if payload["ok"] == false {
        return super::project(payload, super::CatalogProjection::List { limit });
    }
    let Some(entries) = payload["entries"].as_array() else {
        return invalid();
    };
    let fields = [
        "id",
        "uuid",
        "title",
        "aliases",
        "visible",
        "monitor_profile",
        "properties",
    ];
    let entries: Vec<Value> = entries
        .iter()
        .map(|entry| {
            Value::Object(
                fields
                    .iter()
                    .filter_map(|field| {
                        entry
                            .get(*field)
                            .map(|value| ((*field).to_owned(), value.clone()))
                    })
                    .collect(),
            )
        })
        .collect();
    let Some(next_cursor) = payload.get("next_cursor") else {
        return invalid();
    };
    let sanitized = json!({"ok":true,"revision":payload["revision"],"entries":entries,"next_cursor":next_cursor});
    let Ok(page) = serde_json::from_value::<LibraryIdentityInventoryResponse>(sanitized.clone())
    else {
        return invalid();
    };
    if page.validate(limit).is_err() {
        return invalid();
    }
    sanitized
}

fn invalid() -> Value {
    super::error(
        "invalid_catalog_response",
        "catalog returned an invalid identity inventory",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64) -> Value {
        json!({"id":format!("app-{id:016x}"),"uuid":format!("stable-{id}"),"title":"Game","aliases":[format!("app-{:016x}",1000+id)],"visible":false,"monitor_profile":true,"properties":{"remote_monitor":{"layout":"extended"}}})
    }

    fn page(entries: Vec<Value>) -> Value {
        json!({"ok":true,"revision":4,"entries":entries,"next_cursor":null})
    }

    #[test]
    fn complete_identity_projection_keeps_hidden_occupancy_and_strips_launch_fields() {
        let mut entry = row(1);
        entry["launch"] = json!({"program":"/private/game"});
        entry["prep_cmd"] = json!(["private"]);
        let response = project(page(vec![entry]), 32);
        assert_eq!(response["ok"], true);
        assert_eq!(response["entries"][0]["visible"], false);
        assert_eq!(response["entries"][0]["aliases"][0], "app-00000000000003e9");
        assert_eq!(response["entries"][0].as_object().unwrap().len(), 7);
        assert!(response["entries"][0].get("launch").is_none());
        assert_eq!(
            response["entries"][0]["properties"]["remote_monitor"]["layout"],
            "extended"
        );
    }

    #[test]
    fn malformed_page_never_publishes_partial_or_ambiguous_occupancy() {
        let mut invalid_alias = row(2);
        invalid_alias["aliases"] = json!(["42"]);
        let mut oversized_properties = row(2);
        oversized_properties["properties"] = json!({"payload":"x".repeat(4096)});
        for payload in [
            page(vec![row(1), invalid_alias]),
            page(vec![row(1), oversized_properties]),
            page(vec![row(2), row(1)]),
            page(vec![row(1), row(1)]),
            json!({"ok":true,"revision":4,"entries":[row(1)]}),
            json!({"ok":true,"revision":4,"entries":[row(1)],"next_cursor":{"revision":5,"after":"app-0000000000000001","query":""}}),
            json!({"ok":true,"revision":4,"entries":[],"next_cursor":{"revision":4,"after":"app-0000000000000001","query":""}}),
        ] {
            let response = project(payload, 32);
            assert_eq!(response["ok"], false, "{response}");
            assert!(response.get("entries").is_none());
        }
        assert_eq!(project(page(vec![row(1), row(2)]), 1)["ok"], false);
    }
}
