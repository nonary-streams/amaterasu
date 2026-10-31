//! Construct a unified library read and a bounded provider snapshot batch.
//! A real module submits these over RuntimeClient and handles broker results.
use amaterasu_module_sdk::application_library::{
    LIBRARY_SERVICE, LibraryRequest, ProviderEntry, SnapshotBatch,
};
use serde_json::json;

fn main() {
    let request = LibraryRequest::List {
        cursor: None,
        limit: Some(20),
    };
    println!(
        "{}: {}",
        LIBRARY_SERVICE,
        serde_json::to_string(&request).unwrap()
    );

    let batch = SnapshotBatch {
        entries: vec![ProviderEntry {
            source_id: "example-game".into(),
            entry: json!({"name":"Example Game", "launch":{"kind":"application", "path":"/games/example"}}),
            source_flags: None,
        }],
        remove_source_ids: vec![],
        expire_source_ids: vec![],
        selection_remove_source_ids: vec![],
    };
    batch.validate().unwrap();
}
