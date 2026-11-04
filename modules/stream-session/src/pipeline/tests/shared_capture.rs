use super::*;

const OWNER: &str = "amaterasu.rtsp";
const SOURCE_OPERATION: &str = "nvhttp-original-source";

fn target(stream: &str) -> Value {
    json!({"kind":"stream","host_epoch":"epoch","session_id":"application","stream_id":stream,"generation":1})
}

fn host() -> Harness {
    let mut host = Harness::new();
    host.pipeline.settings.allowed_principals.push(OWNER.into());
    host.pipeline.settings.enable_input = false;
    host.broker.lifecycle_success = true;
    host
}

fn preparation(client: u8, shared: bool) -> Value {
    let mut request = prepare();
    request["operation_id"] = json!(format!("external-client-{client}"));
    request["key_hex"] = json!(format!("{client:02x}").repeat(16));
    request["key_id"] = json!(client);
    request["connect_data"] = json!(client);
    request["rtsp_operation_id"] = json!(if shared {
        format!("nvhttp-client-{client}")
    } else {
        SOURCE_OPERATION.into()
    });
    request["lifecycle_capture_mode"] = json!("external");
    request["lifecycle_session_target"] =
        json!({"kind":"session","host_epoch":"epoch","session_id":"application","generation":1});
    if shared {
        request["shared_source_operation_id"] = json!(SOURCE_OPERATION);
    }
    request
}

fn configuration(id: u64, client: u8, shared: bool) -> Value {
    let mut request = start(id);
    request["configuration"]["capture_provider"] = json!("pipewire");
    request["configuration"]["output"] = json!("amaterasu-owned-display");
    request["configuration"]["lifecycle_capture_mode"] = json!("external");
    request["configuration"]["lifecycle_target"] = target(&format!("client-{client}"));
    if shared {
        request["configuration"]["lifecycle_shared_source"] = target("client-1");
        request["configuration"]["configuration"]["width"] = json!(640);
        request["configuration"]["configuration"]["height"] = json!(360);
        request["configuration"]["configuration"]["fps"] = json!(30);
    }
    request
}

fn launch(host: &mut Harness, client: u8, shared: bool) -> u64 {
    let prepared = host.response_from(u64::from(client) * 10, OWNER, preparation(client, shared));
    let id = prepared["session"]
        .as_u64()
        .unwrap_or_else(|| panic!("{prepared}"));
    let started = host.response_from(
        u64::from(client) * 10 + 1,
        OWNER,
        configuration(id, client, shared),
    );
    assert_eq!(started["state"], "active", "{started}");
    id
}

fn display_calls(host: &Harness, action: &str) -> usize {
    host.broker
        .calls
        .iter()
        .filter(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == action
        })
        .count()
}

fn restart(host: &Harness) -> Harness {
    let mut restarted = Harness::with_restart_state(
        serde_json::to_value(&host.pipeline.journal.document).unwrap(),
        host.broker.state_revision,
    );
    restarted
        .pipeline
        .settings
        .allowed_principals
        .push(OWNER.into());
    restarted.pipeline.settings.enable_input = false;
    restarted.broker.lifecycle_success = true;
    restarted.broker.inherit_components(&host.broker);
    restarted.broker.virtual_display_specification =
        host.broker.virtual_display_specification.clone();
    restarted.broker.virtual_display_references = host.broker.virtual_display_references.clone();
    restarted
}

fn restart_in_order(host: &Harness, order: [u64; 2]) -> Harness {
    let mut restarted = restart(host);
    // Load and reconstruct the actual durable records, then select both queue
    // orders explicitly instead of relying on randomly allocated record IDs.
    for _ in 0..100 {
        restarted.now += 1;
        restarted.broker.now = restarted.now;
        restarted
            .pipeline
            .journal
            .progress_load(&mut |command| restarted.broker.send(command), restarted.now);
        if restarted.pipeline.journal.loaded {
            break;
        }
    }
    assert!(restarted.pipeline.journal.loaded);
    restarted
        .pipeline
        .initialize_recovery(restarted.now)
        .unwrap();
    assert_eq!(restarted.pipeline.records.len(), order.len());
    restarted.pipeline.job = None;
    restarted.pipeline.recovery_queue = order.into_iter().collect();
    restarted.pipeline.start_next_recovery(restarted.now);
    restarted
}

#[test]
fn restart_resolves_prepared_join_by_exact_coordinator_preparation_identity() {
    for admitted in [false, true] {
        let mut host = host();
        launch(&mut host, 1, false);
        let prepared = host.response_from(20, OWNER, preparation(2, true));
        let joined = prepared["session"].as_u64().unwrap();
        assert!(host.pipeline.records[&joined].start.is_none());
        let mut restarted = restart(&host);
        if admitted {
            restarted.broker.lifecycle_preparation_resolution = Some(json!({
                "status":"succeeded","target":target("client-2")
            }));
        }
        restarted.idle();
        let query = restarted
            .broker
            .calls
            .iter()
            .find(|call| {
                call.service == "amaterasu.session-lifecycle/1"
                    && call.payload["operation"] == "resolve_stream_preparation"
            })
            .unwrap();
        assert_eq!(
            query.payload["preparation_operation_id"],
            "external-client-2-lifecycle-prepare"
        );
        assert_eq!(query.payload["preparation_caller"], OWNER);
        assert_eq!(
            query.payload["session"],
            preparation(2, true)["lifecycle_session_target"]
        );
        let joined_retired = restarted.broker.calls.iter().any(|call| {
            call.service == "amaterasu.session-lifecycle/1"
                && call.payload["operation"] == "retire_stream"
                && call.payload["target"] == target("client-2")
        });
        assert_eq!(joined_retired, admitted);
        assert!(restarted.pipeline.journal.document.entries.is_empty());
        assert!(restarted.broker.virtual_display_references.is_empty());
        assert_eq!(display_calls(&restarted, "create"), 0);
        assert_eq!(display_calls(&restarted, "remove"), 1);
    }
}

#[test]
fn restart_preserves_shared_source_when_prepared_join_identity_is_unconfirmed() {
    for reply in [
        json!({"status":"rejected"}),
        json!({"status":"succeeded","target":{
            "kind":"stream","host_epoch":"epoch","session_id":"other-application","stream_id":"client-2","generation":1}}),
        json!({"status":"missing","target":null,"preparation_operation_id":"forged-preparation"}),
        json!({"status":"pending","target":null}),
    ] {
        for prepared_first in [false, true] {
            let mut host = host();
            let source = launch(&mut host, 1, false);
            let prepared = host.response_from(20, OWNER, preparation(2, true));
            let joined = prepared["session"].as_u64().unwrap();
            let order = if prepared_first {
                [joined, source]
            } else {
                [source, joined]
            };
            let mut restarted = restart_in_order(&host, order);
            restarted.broker.lifecycle_preparation_resolution = Some(reply.clone());
            restarted.idle();
            assert!(
                restarted
                    .pipeline
                    .journal
                    .document
                    .entries
                    .contains_key(&joined)
            );
            assert!(
                restarted.pipeline.records[&joined]
                    .restart_lifecycle_preparation
                    .is_some()
            );
            assert!(restarted.pipeline.records[&joined].shared_capture_attached);
            assert_eq!(display_calls(&restarted, "remove"), 0);
            assert_eq!(restarted.broker.virtual_display_references.len(), 1);
            // Pending admission stays bounded without dropping its durable group
            // claim when the recovery lookup deadline expires.
            restarted.now += 60_000;
            restarted.idle();
            assert_eq!(
                restarted.pipeline.records[&joined].state,
                State::CleanupFailed
            );
            assert!(restarted.pipeline.records[&joined].shared_capture_attached);
            assert!(
                restarted
                    .pipeline
                    .journal
                    .document
                    .entries
                    .contains_key(&joined)
            );
            assert_eq!(display_calls(&restarted, "remove"), 0);
        }
    }
}

#[test]
fn restart_after_cancelled_prepared_join_recovers_only_the_original_source() {
    let mut host = host();
    launch(&mut host, 1, false);
    let prepared = host.response_from(20, OWNER, preparation(2, true));
    let joined = prepared["session"].as_u64().unwrap();
    assert_eq!(
        host.response_from(21, OWNER, json!({"action":"stop","session":joined}))["state"],
        "stopped"
    );
    let mut restarted = restart(&host);
    restarted.idle();
    assert!(restarted.pipeline.journal.document.entries.is_empty());
    assert!(restarted.broker.virtual_display_references.is_empty());
    assert_eq!(display_calls(&restarted, "remove"), 1);
    assert!(
        !restarted
            .broker
            .calls
            .iter()
            .any(|call| call.payload["operation"] == "resolve_stream_preparation")
    );
}

fn stop(host: &mut Harness, id: u64, client: u8, shared_retained: bool) -> Value {
    host.broker.lifecycle_retirement_override = shared_retained.then(|| json!({
        "status":"succeeded","target":target(&format!("client-{client}")),"plan_revision":null,
        "consumer_retired":true,"resources_released":false,"shared_retained":true,"shared_source":target("client-1") }));
    host.response_from(
        100 + u64::from(client),
        OWNER,
        json!({"action":"stop","session":id}),
    )
}

#[test]
fn external_clients_share_one_native_source_and_last_consumer_removes_it_in_either_close_order() {
    for source_first in [false, true] {
        let mut host = host();
        let source = launch(&mut host, 1, false);
        let second = launch(&mut host, 2, true);
        assert_eq!(display_calls(&host, "create"), 1);
        assert_eq!(host.broker.virtual_display_references.len(), 1);
        assert_eq!(
            host.broker.virtual_display_specification.as_ref().unwrap()["refresh_millihz"],
            60000
        );
        for resource in [Resource::Video, Resource::Control, Resource::Media] {
            assert_ne!(
                host.pipeline.records[&source].resources[&resource],
                host.pipeline.records[&second].resources[&resource]
            );
        }
        let video: Vec<_> = host
            .broker
            .calls
            .iter()
            .filter(|call| {
                call.service == Resource::Video.service() && call.payload["action"] == "open"
            })
            .collect();
        assert_eq!(video.len(), 2);
        assert_eq!(video[0].payload["output"], video[1].payload["output"]);
        assert_eq!(video[1].payload["configuration"]["fps"], 30);
        let (first, first_client, last, last_client) = if source_first {
            (source, 1, second, 2)
        } else {
            (second, 2, source, 1)
        };
        let closed = stop(&mut host, first, first_client, true);
        assert_eq!(closed["state"], "stopped", "{closed}");
        assert_eq!(closed["lifecycle_shared_retained"], true);
        assert_eq!(closed["lifecycle_resources_released"], false);
        assert_eq!(display_calls(&host, "remove"), 0);
        assert_eq!(host.broker.virtual_display_references.len(), 1);
        assert_eq!(host.pipeline.records[&last].state, State::Active);
        let closed = stop(&mut host, last, last_client, false);
        assert_eq!(closed["state"], "stopped", "{closed}");
        assert_eq!(closed["lifecycle_resources_released"], true);
        assert_eq!(display_calls(&host, "remove"), 1);
        assert!(host.broker.virtual_display_references.is_empty());
        assert!(host.broker.virtual_display_specification.is_none());
        assert!(host.pipeline.journal.document.entries.is_empty());
    }
}

#[test]
fn replacement_client_uses_retained_source_after_original_transport_stops() {
    let mut host = host();
    let source = launch(&mut host, 1, false);
    let second = launch(&mut host, 2, true);
    assert_eq!(stop(&mut host, source, 1, true)["state"], "stopped");
    let third = launch(&mut host, 3, true);
    assert_eq!(display_calls(&host, "create"), 1);
    assert_eq!(
        host.pipeline.records[&third]
            .shared_capture
            .as_ref()
            .unwrap()
            .operation_id,
        SOURCE_OPERATION
    );
    assert_eq!(stop(&mut host, second, 2, true)["state"], "stopped");
    assert_eq!(display_calls(&host, "remove"), 0);
    assert_eq!(stop(&mut host, third, 3, false)["state"], "stopped");
    assert_eq!(display_calls(&host, "remove"), 1);
}

#[test]
fn cancelling_prepared_join_preserves_the_source_reference() {
    let mut host = host();
    let source = launch(&mut host, 1, false);
    let prepared = host.response_from(20, OWNER, preparation(2, true));
    let second = prepared["session"].as_u64().unwrap();
    let closed = host.response_from(21, OWNER, json!({"action":"stop","session":second}));
    assert_eq!(closed["state"], "stopped", "{closed}");
    assert_eq!(display_calls(&host, "create"), 1);
    assert_eq!(display_calls(&host, "remove"), 0);
    assert_eq!(host.broker.virtual_display_references.len(), 1);
    assert_eq!(host.pipeline.records[&source].state, State::Active);
}

#[test]
fn lost_native_source_results_reconcile_the_original_reference() {
    let mut host = host();
    host.broker.fault = Some((
        "amaterasu.virtual-display/1:create".into(),
        Fault::AfterMutation,
    ));
    let prepared = host.response_from(10, OWNER, preparation(1, false));
    let source = prepared["session"].as_u64().unwrap();
    let failed = host.response_from(11, OWNER, configuration(source, 1, false));
    assert_eq!(failed["state"], "failed", "{failed}");
    let descriptor = host.pipeline.records[&source]
        .shared_capture
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(display_calls(&host, "create"), 1);
    assert_eq!(display_calls(&host, "remove"), 1);
    assert!(host.broker.virtual_display_references.is_empty());
    assert!(host.pipeline.journal.document.entries.is_empty());
    for call in host.broker.calls.iter().filter(|call| {
        call.service == "amaterasu.virtual-display/1"
            && matches!(call.payload["action"].as_str(), Some("create" | "remove"))
    }) {
        assert_eq!(call.payload["reference_id"], descriptor.reference_id);
    }
    // A lost final Remove reply is also reconciled without another mutation.
    let source = launch(&mut host, 2, false);
    host.broker.fault = Some((
        "amaterasu.virtual-display/1:remove".into(),
        Fault::AfterMutation,
    ));
    assert_eq!(stop(&mut host, source, 2, false)["state"], "stopped");
    assert_eq!(display_calls(&host, "remove"), 2);
    assert!(host.broker.virtual_display_references.is_empty());
    assert!(host.pipeline.journal.document.entries.is_empty());
}

#[test]
fn cancelling_source_start_after_native_commit_retires_the_same_reference() {
    let mut host = host();
    let prepared = host.response_from(10, OWNER, preparation(1, false));
    let source = prepared["session"].as_u64().unwrap();
    host.broker.fault = Some((
        "amaterasu.virtual-display/1:create".into(),
        Fault::AfterMutation,
    ));
    host.broker
        .enqueue(11, OWNER, configuration(source, 1, false));
    for _ in 0..100 {
        host.tick();
        if !host.broker.virtual_display_references.is_empty() {
            host.broker.cancel_parent(11);
            break;
        }
    }
    for _ in 0..200 {
        host.tick();
        if host.pipeline.job.is_none() && host.pipeline.records[&source].state == State::Cancelled {
            break;
        }
    }
    assert_eq!(host.pipeline.records[&source].state, State::Cancelled);
    assert_eq!(display_calls(&host, "create"), 1);
    assert_eq!(display_calls(&host, "remove"), 1);
    assert!(host.broker.virtual_display_references.is_empty());
    assert!(host.broker.virtual_display_specification.is_none());
    assert!(host.pipeline.journal.document.entries.is_empty());
}

#[test]
fn joins_reject_unready_foreign_and_changed_sources_before_capture() {
    let mut host = host();
    let prepared = host.response_from(10, OWNER, preparation(1, false));
    let source = prepared["session"].as_u64().unwrap();
    assert_eq!(
        host.response_from(20, OWNER, preparation(2, true))["code"],
        "shared_capture_source_not_ready"
    );
    assert_eq!(display_calls(&host, "create"), 0);
    assert_eq!(
        host.response_from(11, OWNER, configuration(source, 1, false))["state"],
        "active"
    );
    let mut foreign = preparation(2, true);
    foreign["lifecycle_session_target"]["generation"] = json!(2);
    assert_eq!(
        host.response_from(21, OWNER, foreign)["code"],
        "shared_capture_source_not_ready"
    );
    let second = host.response_from(22, OWNER, preparation(2, true))["session"]
        .as_u64()
        .unwrap();
    host.broker.virtual_display_specification.as_mut().unwrap()["width"] = json!(999);
    let failed = host.response_from(23, OWNER, configuration(second, 2, true));
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(display_calls(&host, "create"), 1);
    assert_eq!(display_calls(&host, "remove"), 0);
    assert_eq!(host.broker.virtual_display_references.len(), 1);
    assert!(
        !host
            .broker
            .calls
            .iter()
            .any(|call| call.service == Resource::Video.service()
                && call.payload["action"] == "open"
                && call.payload["operation_id"] == format!("ss-{second:x}-video"))
    );
}

#[test]
fn shared_source_journal_carries_exact_native_cleanup_reference_for_every_consumer() {
    let mut host = host();
    let first = launch(&mut host, 1, false);
    let second = launch(&mut host, 2, true);
    let document = host.pipeline.journal.document.clone();
    let source = document.entries[&first].shared_capture.as_ref().unwrap();
    assert_eq!(
        document.entries[&second].shared_capture.as_ref(),
        Some(source)
    );
    for entry in document.entries.values() {
        let restored = entry.recovery_record().unwrap();
        assert_eq!(restored.shared_capture.as_ref(), Some(source));
        assert!(restored.shared_capture_attached);
        assert_eq!(
            super::super::virtual_display::owned_remove(&restored),
            source.remove()
        );
    }
    let mut restarted = Harness::with_restart_state(
        serde_json::to_value(document).unwrap(),
        host.broker.state_revision,
    );
    restarted
        .pipeline
        .settings
        .allowed_principals
        .push(OWNER.into());
    restarted.pipeline.settings.enable_input = false;
    restarted.broker.lifecycle_success = true;
    restarted.broker.inherit_components(&host.broker);
    restarted.broker.virtual_display_specification =
        host.broker.virtual_display_specification.clone();
    restarted.broker.virtual_display_references = host.broker.virtual_display_references.clone();
    restarted.idle();
    assert_eq!(display_calls(&restarted, "remove"), 1);
    assert!(restarted.broker.virtual_display_references.is_empty());
    assert!(restarted.pipeline.journal.document.entries.is_empty());
    assert!(
        !restarted
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.virtual-display/1"
                && call.payload["action"] == "create")
    );
}
