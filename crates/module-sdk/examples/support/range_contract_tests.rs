//! Shared conformance checks run separately against both example executors.
use amaterasu_module_sdk::{
    application_library::provider_adapter::{CallerPolicy, Provider, ProviderAdapter},
    requests::IncomingRequest,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn exercise<P: Provider>(provider: P, service: &str) {
    let incarnation = provider.incarnation().to_owned();
    let mut adapter = ProviderAdapter::with_sequenced_policies(
        provider,
        service.into(),
        BTreeMap::from([(
            "launch".into(),
            CallerPolicy {
                initiators: ["nvhttp".into(), "launch".into()].into(),
                lifecycle: true,
                ..Default::default()
            },
        )]),
    )
    .unwrap();
    let mut dispatch = |initiator: &str, payload: Value| {
        adapter.dispatch(&IncomingRequest {
            id: 1,
            caller: "launch".into(),
            initiator: initiator.into(),
            service: service.into(),
            payload,
            remaining_ms: 10_000,
        })
    };
    let capability = dispatch("launch", json!({"action":"limiter_target_capabilities_v2"}));
    assert_eq!(capability["supported"], true, "{capability}");
    for sequence in 1..=5000u64 {
        let position = json!({"generation":"fixture-generation","sequence":sequence});
        let operation = format!("lim2.fixture-generation.{sequence:016x}");
        let policy = json!({"source":"library","version":1,"application":{"library":"games","id":"opaque"},"entry_revision":"entry","policy_revision":"policy","policy_source":"authority","snapshot_id":"snapshot"});
        let target = json!({"version":1,"stage":"reserve","operation_id":operation,"application_id":"fixture-game","provider_incarnation":incarnation,"ticket":null,"policy_identity":policy});
        let reservation =
            json!({"version":2,"position":position,"target":target,"original_initiator":"nvhttp"});
        let reserve = dispatch(
            "nvhttp",
            json!({"action":"attest_limiter_target_v2","request":reservation}),
        );
        assert_eq!(reserve["ok"], true, "{sequence}: {reserve}");
        let launch_request = json!({"version":2,"action":"launch","reservation":reservation});
        let launched = dispatch(
            "nvhttp",
            json!({"action":"limiter_lifecycle_v2","request":launch_request}),
        );
        assert_eq!(
            launched["lifecycle"]["observation"]["state"], "running",
            "{launched}"
        );
        let fence = json!({"version":2,"generation":"fixture-generation","through":sequence,"provider_incarnation":incarnation,"original_initiator":"nvhttp"});
        assert_ne!(
            dispatch(
                "launch",
                json!({"action":"fence_limiter_targets_v2","request":fence})
            )["ok"],
            true
        );
        let stopped = dispatch(
            "launch",
            json!({"action":"limiter_lifecycle_v2","request":{"version":2,"action":"recover_stop","reservation":reservation}}),
        );
        assert_eq!(
            stopped["lifecycle"]["observation"]["state"], "stopped",
            "{stopped}"
        );
        let retirement = json!({"version":2,"position":position,"retirement":{"version":1,"operation_id":operation,"application_id":"fixture-game","provider_incarnation":incarnation,"original_initiator":"nvhttp","policy_identity":policy}});
        let retired = dispatch(
            "launch",
            json!({"action":"retire_limiter_target_v2","request":retirement}),
        );
        assert_eq!(retired["retirement"]["retired"], true, "{retired}");
        let proof = dispatch(
            "launch",
            json!({"action":"fence_limiter_targets_v2","request":fence}),
        );
        assert_eq!(proof["fence"]["fenced"], true, "{proof}");
        assert_eq!(
            dispatch(
                "launch",
                json!({"action":"fence_limiter_targets_v2","request":fence})
            )["fence"]["fenced"],
            true
        );
        assert_ne!(
            dispatch(
                "nvhttp",
                json!({"action":"attest_limiter_target_v2","request":reservation})
            )["ok"],
            true
        );
        assert_ne!(
            dispatch(
                "nvhttp",
                json!({"action":"limiter_lifecycle_v2","request":launch_request})
            )["ok"],
            true
        );
        assert_ne!(
            dispatch(
                "nvhttp",
                json!({"action":"launch","operation_id":operation,"application_id":"fixture-game","provider_incarnation":incarnation})
            )["ok"],
            true
        );
        let mut foreign = fence.clone();
        foreign["original_initiator"] = json!("foreign");
        assert_ne!(
            dispatch(
                "launch",
                json!({"action":"fence_limiter_targets_v2","request":foreign})
            )["ok"],
            true
        );
        let mut stale = fence.clone();
        stale["provider_incarnation"] = json!("stale-incarnation");
        assert_ne!(
            dispatch(
                "launch",
                json!({"action":"fence_limiter_targets_v2","request":stale})
            )["ok"],
            true
        );
        let mut changed = retirement.clone();
        changed["retirement"]["policy_identity"]["snapshot_id"] = json!("foreign-snapshot");
        assert_ne!(
            dispatch(
                "launch",
                json!({"action":"retire_limiter_target_v2","request":changed})
            )["ok"],
            true
        );
    }
}
