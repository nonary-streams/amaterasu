//! Desktop entries: manual create, normalized readback and export, plus the
//! first-run seed, which is created once, durable across restarts, and never
//! resurrected after the user deletes or renames it.
use super::*;

const OWNER: &str = "amaterasu.management";
const COLLECTION: &str = "principal/amaterasu.management";

fn unseeded_pipeline() -> Pipeline {
    Pipeline::new_with_steam_id_export_principals(
        BTreeSet::from(["amaterasu.launch".into()]),
        BTreeMap::from([("amaterasu.nvhttp".into(), OWNER.into())]),
        BTreeMap::new(),
        BTreeMap::from([("amaterasu.steam".into(), OWNER.into())]),
    )
}

fn seeded_pipeline() -> Pipeline {
    unseeded_pipeline().with_desktop_seed([OWNER.to_owned()])
}

/// Drive the pipeline until the seed settles. Seed requests are parentless;
/// an optional ordinary request is delivered on the first poll.
fn drive_seed(
    backend: &mut Backend,
    pipeline: &mut Pipeline,
    mut incoming: Option<Value>,
) -> (Vec<Value>, Option<Value>) {
    let expects_reply = incoming.is_some();
    let mut seed_requests = Vec::new();
    let mut reply = None;
    let mut state_result = None;
    for _ in 0..200 {
        pipeline
            .tick(|command| {
                Ok(match command {
                    Command::Poll {} => {
                        incoming
                            .take()
                            .map_or(Response::Pending, |payload| Response::Request {
                                id: 7,
                                caller: OWNER.into(),
                                initiator: OWNER.into(),
                                service: crate::SERVICE.into(),
                                remaining_ms: 30_000,
                                payload,
                            })
                    }
                    Command::Request {
                        payload,
                        parent,
                        operation_id,
                        ..
                    } => {
                        assert_eq!(
                            payload
                                .get("collection")
                                .or_else(|| payload.get("key"))
                                .unwrap(),
                            COLLECTION
                        );
                        if parent.is_none() {
                            assert!(operation_id.starts_with("catalog.seed."));
                            seed_requests.push(payload.clone());
                        } else {
                            assert_eq!(parent, Some(7));
                        }
                        let is_commit = payload["action"] == "collection_commit";
                        let result = backend.handle(payload);
                        state_result = Some(if is_commit && backend.lose_commit_ack_once {
                            backend.lose_commit_ack_once = false;
                            error("deadline_exceeded", "injected lost commit acknowledgement")
                        } else {
                            result
                        });
                        Response::Accepted { id: 20 }
                    }
                    Command::TakeResult { .. } => Response::Result {
                        payload: state_result.take().unwrap(),
                    },
                    Command::Complete { payload, .. } => {
                        reply = Some(payload);
                        Response::Ok
                    }
                    command => panic!("unexpected command: {command:?}"),
                })
            })
            .unwrap();
        if pipeline.seed.is_finished() && (!expects_reply || reply.is_some()) {
            break;
        }
    }
    (seed_requests, reply)
}

fn seed(backend: &mut Backend) -> Pipeline {
    let mut pipeline = seeded_pipeline();
    drive_seed(backend, &mut pipeline, None);
    assert!(pipeline.seed.is_finished());
    pipeline
}

fn management(backend: &mut Backend, pipeline: &mut Pipeline, payload: Value) -> Value {
    backend.run_for(pipeline, payload, OWNER, OWNER, OWNER)
}

fn desktop_rows(backend: &Backend) -> Vec<(&String, &Value)> {
    backend
        .entries
        .iter()
        .filter(|(key, _)| key.starts_with("app-"))
        .collect()
}

#[test]
fn fresh_catalog_gets_one_desktop_entry_and_a_hidden_marker_in_one_commit() {
    let mut backend = Backend::default();
    let mut pipeline = seeded_pipeline();

    let (requests, _) = drive_seed(&mut backend, &mut pipeline, None);

    let commits: Vec<_> = requests
        .iter()
        .filter(|request| request["action"] == "collection_commit")
        .collect();
    assert_eq!(commits.len(), 1, "one atomic commit: {requests:?}");
    assert_eq!(commits[0]["expected_revision"], 0);
    assert_eq!(backend.revision, 1);
    let rows = desktop_rows(&backend);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "app-0000000000000001");
    assert_eq!(rows[0].1["name"], "Desktop");
    assert_eq!(
        rows[0].1["launch"],
        json!({"kind":"desktop","exit_timeout_seconds":10})
    );
    assert_eq!(
        backend.entries[crate::seed::MARKER_KEY],
        json!({"version":1,"entry_id":"app-0000000000000001"})
    );
    assert_eq!(
        backend.metadata,
        json!({"revision":1,"next_id":2,"provider_count":1})
    );

    // The hidden marker is invisible to list, search and the sanitized export.
    let listed = management(&mut backend, &mut pipeline, json!({"action":"list"}));
    assert_eq!(listed["revision"], 1, "{listed}");
    assert_eq!(listed["entries"].as_array().unwrap().len(), 1);
    assert_eq!(listed["entries"][0]["entry"]["name"], "Desktop");
    let found = management(
        &mut backend,
        &mut pipeline,
        json!({"action":"search","query":"desk"}),
    );
    assert_eq!(found["entries"].as_array().unwrap().len(), 1);
    let exported = backend.run_for(
        &mut pipeline,
        json!({"action":"export_list"}),
        "amaterasu.nvhttp",
        "amaterasu.nvhttp",
        OWNER,
    );
    assert_eq!(
        exported["entries"],
        json!([{"id":"app-0000000000000001","name":"Desktop"}])
    );

    // Launch resolution reads the same typed target.
    let resolved = backend.run_for(
        &mut pipeline,
        json!({"action":"resolve_launch","id":"app-0000000000000001"}),
        "amaterasu.launch",
        OWNER,
        OWNER,
    );
    assert_eq!(resolved["ok"], true, "{resolved}");
    assert_eq!(resolved["entry"]["launch"]["kind"], "desktop");

    // Later writes keep the ID counter and the hidden-row accounting coherent.
    let created = management(
        &mut backend,
        &mut pipeline,
        json!({"action":"create","expected_revision":1,"entry":entry("Second")}),
    );
    assert_eq!(created["id"], "app-0000000000000002", "{created}");
    assert!(backend.entries.contains_key(crate::seed::MARKER_KEY));
}

#[test]
fn restart_does_not_seed_again() {
    let mut backend = Backend::default();
    seed(&mut backend);
    let revision = backend.revision;
    let entries = backend.entries.clone();

    for _ in 0..3 {
        let mut restarted = seeded_pipeline();
        let (requests, _) = drive_seed(&mut backend, &mut restarted, None);
        assert!(
            requests
                .iter()
                .all(|request| request["action"] != "collection_commit"),
            "a restart wrote: {requests:?}"
        );
    }
    assert_eq!(backend.revision, revision);
    assert_eq!(backend.entries, entries);
}

#[test]
fn deleted_desktop_entry_is_not_recreated_after_restart() {
    let mut backend = Backend::default();
    let mut pipeline = seed(&mut backend);
    let deleted = management(
        &mut backend,
        &mut pipeline,
        json!({"action":"delete","id":"app-0000000000000001","expected_revision":1}),
    );
    assert_eq!(deleted["ok"], true, "{deleted}");
    assert!(desktop_rows(&backend).is_empty());
    let revision = backend.revision;

    let mut restarted = seeded_pipeline();
    drive_seed(&mut backend, &mut restarted, None);

    assert!(desktop_rows(&backend).is_empty(), "the seed resurrected");
    assert_eq!(backend.revision, revision);
    let listed = management(&mut backend, &mut restarted, json!({"action":"list"}));
    assert_eq!(listed["entries"], json!([]));
}

#[test]
fn renamed_desktop_entry_stays_alone_after_restart() {
    let mut backend = Backend::default();
    let mut pipeline = seed(&mut backend);
    let mut renamed = backend.entries["app-0000000000000001"].clone();
    renamed["name"] = json!("My PC");
    let updated = management(
        &mut backend,
        &mut pipeline,
        json!({"action":"update","id":"app-0000000000000001","expected_revision":1,"entry":renamed}),
    );
    assert_eq!(updated["ok"], true, "{updated}");

    let mut restarted = seeded_pipeline();
    drive_seed(&mut backend, &mut restarted, None);

    let rows = desktop_rows(&backend);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1["name"], "My PC");
}

#[test]
fn existing_catalog_gains_one_desktop_entry_and_provider_accounting_stays_valid() {
    let mut backend = Backend::default();
    let mut plain = Pipeline::default();
    let created = management(
        &mut backend,
        &mut plain,
        json!({"action":"create","expected_revision":0,"entry":entry("Existing")}),
    );
    assert_eq!(created["ok"], true, "{created}");

    let mut pipeline = seed(&mut backend);

    assert_eq!(desktop_rows(&backend).len(), 2);
    assert_eq!(backend.entries["app-0000000000000002"]["name"], "Desktop");
    assert_eq!(
        backend.metadata,
        json!({"revision":2,"next_id":3,"provider_count":1})
    );
    // A provider scan counts hidden rows against provider_count, and must
    // accept the seed marker as one.
    let begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("scan-after-seed", 1),
        "amaterasu.steam",
        OWNER,
        OWNER,
    );
    assert_eq!(begin["ok"], true, "{begin}");
}

#[test]
fn catalog_with_an_unmigrated_legacy_document_waits_for_migration() {
    let mut backend = Backend {
        legacy: Some(json!({
            "revision":1,"next_id":2,
            "entries":{"app-0000000000000001":entry("Legacy")},
        })),
        ..Backend::default()
    };

    seed(&mut backend);

    assert_eq!(
        backend.revision, 0,
        "seeding must not fork a legacy catalog"
    );
    assert!(backend.entries.is_empty());
}

#[test]
fn seeding_is_off_unless_configured() {
    let mut backend = Backend::default();
    let mut pipeline = Pipeline::new_with_steam_id_export_principals(
        BTreeSet::new(),
        BTreeMap::from([("amaterasu.nvhttp".into(), OWNER.into())]),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    assert!(!pipeline.seed.needs_turn());
    let (requests, _) = drive_seed(&mut backend, &mut pipeline, None);
    assert!(requests.is_empty());
    assert_eq!(backend.revision, 0);
}

#[test]
fn lost_commit_acknowledgement_reconciles_by_marker_without_a_second_entry() {
    let mut backend = Backend {
        lose_commit_ack_once: true,
        ..Backend::default()
    };
    let mut pipeline = seeded_pipeline();

    drive_seed(&mut backend, &mut pipeline, None);
    assert!(!pipeline.seed.is_finished(), "a retry must be scheduled");
    assert_eq!(desktop_rows(&backend).len(), 1, "the commit did land");
    pipeline.seed.expedite_retry();
    drive_seed(&mut backend, &mut pipeline, None);

    assert!(pipeline.seed.is_finished());
    assert_eq!(desktop_rows(&backend).len(), 1);
    assert_eq!(backend.revision, 1);
}

#[test]
fn failed_commit_retries_a_bounded_number_of_times_then_leaves_the_catalog_unseeded() {
    let mut backend = Backend {
        fail: true,
        ..Backend::default()
    };
    let mut pipeline = seeded_pipeline();

    let mut rounds = 0;
    while !pipeline.seed.is_finished() {
        drive_seed(&mut backend, &mut pipeline, None);
        pipeline.seed.expedite_retry();
        rounds += 1;
        assert!(rounds <= 20, "retries are unbounded");
    }

    assert!(rounds > 1, "a failed commit must be retried");
    assert!(desktop_rows(&backend).is_empty());
    assert!(!backend.entries.contains_key(crate::seed::MARKER_KEY));

    // The marker is still absent, so the next start seeds normally.
    backend.fail = false;
    seed(&mut backend);
    assert_eq!(desktop_rows(&backend).len(), 1);
}

#[test]
fn ordinary_requests_wait_for_the_seed_and_see_its_entry() {
    let mut backend = Backend::default();
    let mut pipeline = seeded_pipeline();

    let (_, reply) = drive_seed(&mut backend, &mut pipeline, Some(json!({"action":"list"})));

    let reply = reply.expect("the queued list was answered");
    assert_eq!(reply["entries"].as_array().unwrap().len(), 1, "{reply}");
    assert_eq!(reply["entries"][0]["entry"]["name"], "Desktop");
}

#[test]
fn manual_desktop_entry_is_normalized_stored_exported_and_strictly_validated() {
    let mut backend = Backend::default();
    let mut pipeline = unseeded_pipeline();
    let created = management(
        &mut backend,
        &mut pipeline,
        json!({"action":"create","expected_revision":0,"entry":{
            "name":"Streaming desktop",
            "launch":{"kind":"desktop","preparation":[{
                "run":{"executable":"/usr/bin/configure","args":["on"]},
                "undo":{"executable":"/usr/bin/configure","args":["off"]},
            }]},
        }}),
    );
    assert_eq!(created["ok"], true, "{created}");
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["entry"]["launch"]["exit_timeout_seconds"], 10);
    let stored = backend.entries[id].clone();
    assert_eq!(stored["launch"]["kind"], "desktop");
    assert_eq!(
        stored["launch"]["preparation"][0]["undo"]["timeout_ms"],
        10_000
    );
    let reread = management(&mut backend, &mut pipeline, json!({"action":"get","id":id}));
    assert_eq!(reread["entry"], stored);
    let exported = backend.run_for(
        &mut pipeline,
        json!({"action":"export_list"}),
        "amaterasu.nvhttp",
        "amaterasu.nvhttp",
        OWNER,
    );
    assert_eq!(
        exported["entries"],
        json!([{"id":id,"name":"Streaming desktop"}])
    );

    for launch in [
        json!({"kind":"desktop","program":"/usr/bin/true"}),
        json!({"kind":"desktop","path":"/usr/bin/true"}),
        json!({"kind":"desktop","game_id":"a1b2c3d4-1234-5678-9abc-0123456789ab"}),
        json!({"kind":"desktop","steam_id":"480"}),
        json!({"kind":"desktop","elevated":true}),
    ] {
        let expected_revision = backend.revision;
        let rejected = management(
            &mut backend,
            &mut pipeline,
            json!({"action":"create","expected_revision":expected_revision,"entry":{"name":"Bad","launch":launch}}),
        );
        assert_eq!(rejected["ok"], false, "{launch}: {rejected}");
    }
    for entry in [
        json!({"name":"Bad","elevated":true,"launch":{"kind":"desktop"}}),
        json!({"name":"Bad","steam_id":"480","launch":{"kind":"desktop"}}),
    ] {
        let expected_revision = backend.revision;
        let rejected = management(
            &mut backend,
            &mut pipeline,
            json!({"action":"create","expected_revision":expected_revision,"entry":entry}),
        );
        assert_eq!(rejected["error"]["code"], "invalid_entry", "{entry}");
    }
}

#[test]
fn provider_snapshots_cannot_import_a_desktop_entry() {
    let mut backend = Backend::default();
    let mut pipeline = unseeded_pipeline();
    let begin = backend.run_for(
        &mut pipeline,
        snapshot_begin("desktop-import", 1),
        "amaterasu.steam",
        OWNER,
        OWNER,
    );
    assert_eq!(begin["ok"], true, "{begin}");
    let rejected = backend.run_for(
        &mut pipeline,
        snapshot_batch(
            "desktop-import",
            begin["generation"].as_u64().unwrap(),
            1,
            0,
            begin["revision"].as_u64().unwrap(),
            vec![(
                "steam:480",
                json!({"name":"Imported desktop","launch":{"kind":"desktop"}}),
            )],
        ),
        "amaterasu.steam",
        OWNER,
        OWNER,
    );
    assert_eq!(rejected["error"]["code"], "invalid_entry", "{rejected}");
}
