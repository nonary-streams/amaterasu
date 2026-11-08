//! Admission must depend on the new stream, not success of an older cleanup.
use super::*;

fn cleanup_operations() -> BTreeSet<(String, String)> {
    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(host.response(2, start(session))["state"], "active");
    let providers: BTreeSet<_> = host.pipeline.records[&session]
        .resources
        .keys()
        .map(|resource| {
            host.pipeline.records[&session]
                .service(*resource)
                .to_owned()
        })
        .collect();
    let before = host.broker.calls.len();
    assert_eq!(
        host.response(3, json!({"action":"stop", "session":session}))["state"],
        "stopped"
    );
    let operations: BTreeSet<_> = host.broker.calls[before..]
        .iter()
        .filter_map(|call| {
            let action = call.payload["action"].as_str()?;
            matches!(action, "close" | "stop" | "finish_stop")
                .then(|| (call.service.clone(), action.to_owned()))
        })
        .collect();
    for provider in providers {
        assert!(
            operations.iter().any(|(service, _)| service == &provider),
            "provider {provider} has no injected cleanup case"
        );
    }
    operations
}

fn start_fresh(host: &mut Harness, generation: u64) -> u64 {
    let mut request = prepare();
    request["operation_id"] = json!(format!("independent-stream-{generation}"));
    let prepared = host.response(generation * 10 + 1, request);
    assert_eq!(
        prepared["state"], "prepared",
        "new stream blocked: {prepared}"
    );
    let session = prepared["session"].as_u64().unwrap();
    let active = host.response(generation * 10 + 2, start(session));
    assert_eq!(
        active["state"], "active",
        "new stream failed to start: {active}"
    );
    session
}

#[test]
fn generic_cleanup_failure_never_blocks_a_new_stream() {
    // Discover the selected providers' cleanup calls from an actual successful
    // lifecycle. This automatically covers new cleanup providers using the same
    // contracts and never depends on a particular provider error string.
    let operations = cleanup_operations();
    assert!(!operations.is_empty());
    for fault in operations {
        for restart in [false, true] {
            let mut host = Harness::new();
            let mut failed = Vec::new();
            let capacity = host.pipeline.settings.max_live_sessions as u64;
            for generation in 1..=capacity {
                let session = start_fresh(&mut host, generation);
                host.broker.cleanup_failure = Some(fault.clone());
                let result = host.response(
                    generation * 10 + 3,
                    json!({"action":"stop", "session":session}),
                );
                assert_eq!(
                    result["state"], "cleanup_failed",
                    "fault {fault:?} was not exercised: {result}"
                );
                failed.push(session);
            }
            if restart {
                let mut recovered = Harness::with_restart_state(
                    host.broker.state_value.clone().unwrap(),
                    host.broker.state_revision,
                );
                recovered.broker.inherit_components(&host.broker);
                recovered.broker.cleanup_failure = Some(fault.clone());
                recovered.idle();
                host = recovered;
            }
            let fresh = start_fresh(&mut host, capacity + 1);
            for old in failed {
                assert_ne!(old, fresh, "a new stream reused the failed stream identity");
                assert_eq!(host.pipeline.records[&old].state, State::CleanupFailed);
                assert!(
                    host.pipeline.journal.document.entries.contains_key(&old),
                    "cleanup evidence was discarded"
                );
                for (resource, handle) in &host.pipeline.records[&old].resources {
                    assert_ne!(
                        host.pipeline.records[&fresh].resources.get(resource),
                        Some(handle),
                        "new stream reused an unretired provider resource"
                    );
                }
            }
            assert!(host.pipeline.journal.document.entries.contains_key(&fresh));
        }
    }
}
