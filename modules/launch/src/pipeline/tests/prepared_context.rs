use super::*;
use amaterasu_module_sdk::extension_api as api;

fn target() -> Value {
    json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1})
}

fn plan(provider: &str, value: &str) -> Value {
    json!({"provider":provider,"plan":{"plan_id":"env-plan","revision":1,
        "resources":[],"mutation_domains":[],"required_capabilities":[api::LAUNCH_ENVIRONMENT_CAPABILITY],
        "constraints":{"launch_environment":{"schema":api::launch_environment_schema(),
            "contribution":{"edits":{"EXAMPLE_PROFILE":value},"extensions":{}}}},
        "obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,
        "allowed_degradation":[],"control_proposals":[]}})
}

fn request() -> Value {
    let mut request = launch_export("prepared-environment");
    request["preparation_context"] = target();
    request
}

#[test]
fn unchanged_launch_consumer_accepts_another_provider_and_preserves_the_base() {
    for provider in ["example.original", "example.replacement"] {
        let mut harness = Harness::new();
        harness.prepared_context =
            Some(json!({"status":"succeeded","target":target(),"plans":[plan(provider,"chosen")]}));
        let base = BTreeMap::from([("BASE_VALUE".into(), "preserved".into())]);
        let mut pipeline = Pipeline::new(base.clone()).unwrap();
        pipeline.set_prepared_context_service(Some("example.context/1".into()));
        let response = harness.request(&mut pipeline, "alice", request());
        assert_eq!(response["ok"], true, "{response}");
        let read = harness
            .commands
            .iter()
            .position(|(service, _, _)| service == "example.context/1")
            .unwrap();
        let spawn = harness
            .commands
            .iter()
            .position(|(_, payload, _)| payload["action"] == "launch")
            .unwrap();
        assert!(read < spawn);
        assert_eq!(harness.commands[read].1["owner"], "alice");
        assert_eq!(
            harness.commands[read].1["application_operation_id"],
            "prepared-environment"
        );
        assert_eq!(
            harness.commands[spawn].1["environment"]["BASE_VALUE"],
            "preserved"
        );
        assert_eq!(
            harness.commands[spawn].1["environment"]["EXAMPLE_PROFILE"],
            "chosen"
        );
        assert_eq!(pipeline.environment, base);
        // The admitted environment is frozen; a retry does not re-read a changed provider result.
        harness.prepared_context = Some(json!({"status":"rejected"}));
        let retry = harness.request(&mut pipeline, "alice", request());
        assert_eq!(retry, response);
        assert_eq!(
            harness
                .commands
                .iter()
                .filter(|(service, _, _)| service == "example.context/1")
                .count(),
            1
        );
    }
}

#[test]
fn unavailable_mismatched_or_conflicting_context_prevents_process_effects() {
    for response in [
        json!({"status":"rejected"}),
        json!({"status":"succeeded","target":{"kind":"session","host_epoch":"epoch","session_id":"other","generation":1},"plans":[]}),
        json!({"status":"succeeded","target":target(),"plans":[plan("one","first"),plan("two","second")]}),
    ] {
        let mut harness = Harness::new();
        harness.prepared_context = Some(response);
        let mut pipeline = pipeline();
        pipeline.set_prepared_context_service(Some("example.context/1".into()));
        let result = harness.request(&mut pipeline, "alice", request());
        assert_eq!(
            result["launch"]["failure"], "preparation_context_unverified",
            "{result}"
        );
        assert!(harness.sessions.is_empty());
        assert!(harness.processes.is_empty());
    }
    let mut harness = Harness::new();
    let result = harness.request(&mut pipeline(), "alice", request());
    assert_eq!(result["code"], "preparation_context_not_configured");
    assert!(harness.commands.is_empty());
}
