//! Opt-in integration with an independently built display settings library.
use super::*;
use amaterasu_contracts::{ApiMethodApproval, Grants};
use amaterasu_module_sdk::requests::{ApiRequestOptions, RequestOptions};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};

#[test]
#[ignore = "requires an independently built display plugin and its example metadata"]
fn external_display_settings_round_trip_through_broker() {
    let library_path =
        PathBuf::from(std::env::var_os("AMATERASU_DISPLAY_PLUGIN").expect("plugin path"));
    let examples =
        PathBuf::from(std::env::var_os("AMATERASU_DISPLAY_EXAMPLES").expect("examples path"));
    let approvals_path =
        PathBuf::from(std::env::var_os("AMATERASU_DISPLAY_APPROVALS").expect("approvals path"));
    let approvals_json: Value =
        serde_json::from_slice(&std::fs::read(approvals_path).unwrap()).unwrap();
    let approvals: Vec<ApiMethodApproval> =
        serde_json::from_value(approvals_json["api_methods"].clone()).unwrap();
    let approval = approvals
        .iter()
        .find(|entry| entry.method.method == "settings_validate")
        .unwrap()
        .clone();
    let descriptor: Value =
        serde_json::from_slice(&std::fs::read(examples.join("settings-descriptor.json")).unwrap())
            .unwrap();
    let values: BTreeMap<String, Value> = descriptor["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| {
            (
                option["id"].as_str().unwrap().to_owned(),
                option["definition"]["default"].clone(),
            )
        })
        .collect();
    let service = approval.method.service.clone();
    let configuration = RuntimeConfig {
        principals: [
            (
                approval.provider.clone(),
                Grants {
                    provide: [service.clone()].into(),
                    call: ["amaterasu.session-lifecycle/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "amaterasu.extension-registry".into(),
                Grants {
                    call: [service.clone()].into(),
                    provide: ["amaterasu.extension-registry/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "amaterasu.management".into(),
                Grants {
                    call: [service.clone(), "amaterasu.extension-registry/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "amaterasu.session-lifecycle".into(),
                Grants {
                    call: [service.clone()].into(),
                    provide: ["amaterasu.session-lifecycle/1".into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        api_methods: approvals,
        ..Default::default()
    };
    if let Err(reason) = Broker::new(configuration.clone()) {
        panic!("invalid fixture configuration: {reason}");
    }
    let config_bytes = serde_json::to_vec(&configuration).unwrap();
    let mut runtime = std::ptr::null_mut();
    // SAFETY: the input and output live across this synchronous bootstrap call.
    assert_eq!(
        unsafe { create_runtime(BorrowedBytes::new(&config_bytes), &mut runtime) },
        STATUS_OK
    );
    let provider_endpoint = unsafe { endpoint(runtime, &approval.provider) };
    let registry_endpoint = unsafe { endpoint(runtime, "amaterasu.extension-registry") };
    let management_endpoint = unsafe { endpoint(runtime, "amaterasu.management") };
    let lifecycle_endpoint = unsafe { endpoint(runtime, "amaterasu.session-lifecycle") };
    // SAFETY: this opt-in test loads the explicitly supplied trusted test artifact.
    let library = unsafe { libloading::Library::new(library_path) }.unwrap();
    // SAFETY: the plugin exports the documented module entry point, and the library
    // remains loaded until after destroy has retired the module instance.
    let entry: libloading::Symbol<ModuleEntry> =
        unsafe { library.get(b"amaterasu_module_v1\0") }.unwrap();
    let api = unsafe { &*entry() };
    assert!(api.header.supports::<ModuleApi>());
    let mut instance = std::ptr::null_mut();
    assert_eq!(
        unsafe { (api.create)(&provider_endpoint, &mut instance) },
        STATUS_OK
    );
    assert!(!instance.is_null());
    // SAFETY: runtime outlives client and all synchronous calls.
    let client = unsafe { RuntimeClient::from_endpoint(registry_endpoint) };
    let management = unsafe { RuntimeClient::from_endpoint(management_endpoint) };
    let lifecycle = unsafe { RuntimeClient::from_endpoint(lifecycle_endpoint) };
    lifecycle
        .require_success(Command::Register {
            service: "amaterasu.session-lifecycle/1".into(),
        })
        .unwrap();
    client
        .require_success(Command::Register {
            service: "amaterasu.extension-registry/1".into(),
        })
        .unwrap();
    let request = client
        .call_api_serialized(
            ApiRequestOptions {
                request: RequestOptions {
                    service: &service,
                    timeout_ms: 5_000,
                    operation_id: "smoke.validate",
                    parent: None,
                },
                method: &approval.method.method,
                interface_major: approval.method.interface_major,
                interface_minor: approval.method.interface_minor,
            },
            &json!({"values": values.clone()}),
        )
        .unwrap();
    assert!(client.take_serialized::<Value>(request).unwrap().is_none());
    // SAFETY: the instance is owned by the loaded module and remains live.
    assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
    assert_eq!(
        client.take_serialized::<Value>(request).unwrap(),
        Some(json!({"ok":true,"valid":true}))
    );
    if approval.callers.contains("amaterasu.management") {
        let direct = management
            .call_api_serialized(
                ApiRequestOptions {
                    request: RequestOptions {
                        service: &service,
                        timeout_ms: 5_000,
                        operation_id: "smoke.direct",
                        parent: None,
                    },
                    method: &approval.method.method,
                    interface_major: 1,
                    interface_minor: 0,
                },
                &json!({"values": values.clone()}),
            )
            .unwrap();
        assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
        assert_eq!(
            management.take_serialized::<Value>(direct).unwrap(),
            Some(json!({"ok":true,"valid":false,"message":"unauthorized caller"}))
        );
        let parent = management
            .request_serialized(
                RequestOptions {
                    service: "amaterasu.extension-registry/1",
                    timeout_ms: 5_000,
                    operation_id: "smoke.parent",
                    parent: None,
                },
                &json!({}),
            )
            .unwrap();
        let incoming = client.poll_serialized().unwrap().unwrap();
        assert_eq!(incoming.id, parent);
        assert_eq!(incoming.initiator, "amaterasu.management");
        let delegated = client
            .call_api_serialized(
                ApiRequestOptions {
                    request: RequestOptions {
                        service: &service,
                        timeout_ms: 5_000,
                        operation_id: "smoke.delegated",
                        parent: Some(parent),
                    },
                    method: &approval.method.method,
                    interface_major: 1,
                    interface_minor: 0,
                },
                &json!({"values": values.clone()}),
            )
            .unwrap();
        assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
        assert_eq!(
            client.take_serialized::<Value>(delegated).unwrap(),
            Some(json!({"ok":true,"valid":true}))
        );
    }
    let invalid_shape = client.call_api_serialized(
        ApiRequestOptions {
            request: RequestOptions {
                service: &service,
                timeout_ms: 5_000,
                operation_id: "smoke.invalid-shape",
                parent: None,
            },
            method: &approval.method.method,
            interface_major: 1,
            interface_minor: 0,
        },
        &json!({"values":"not an object"}),
    );
    assert!(invalid_shape.unwrap_err().starts_with("invalid_request:"));
    let invalid_values = client
        .call_api_serialized(
            ApiRequestOptions {
                request: RequestOptions {
                    service: &service,
                    timeout_ms: 5_000,
                    operation_id: "smoke.invalid-values",
                    parent: None,
                },
                method: &approval.method.method,
                interface_major: 1,
                interface_minor: 0,
            },
            &json!({"values": {}}),
        )
        .unwrap();
    assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
    let result = client
        .take_serialized::<Value>(invalid_values)
        .unwrap()
        .unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["valid"], false);
    let mut provider_request: amaterasu_module_sdk::extension_api::ProviderRequest =
        serde_json::from_value(json!({
            "provider_sequence": 1, "operation": "resource_plan", "version": 1,
            "transition_id": "transition-1", "operation_id": "plan-1",
            "target": {"kind":"validation", "host_epoch":"epoch-1", "validation_id":"validation-1", "generation":1},
            "phase":"stream.prepare", "settings_revision":"revision-1", "settings":{},
            "intent":null, "reason":null, "requested_video":null,
            "consumer_requirements":{"execution_domain":"desktop", "capture_providers":["capture"], "selector_schemas":["selector/1"]},
            "remaining_ms":5000, "plan_revision":null, "admission_id":null, "approved_plan":null
        })).unwrap();
    let call_provider = |method: &str, operation_id: &str, payload: &Value| {
        lifecycle.call_api_serialized(
            ApiRequestOptions {
                request: RequestOptions {
                    service: &service,
                    timeout_ms: 5_000,
                    operation_id,
                    parent: None,
                },
                method,
                interface_major: 1,
                interface_minor: 0,
            },
            payload,
        )
    };
    let mut missing_sequence = serde_json::to_value(&provider_request).unwrap();
    missing_sequence
        .as_object_mut()
        .unwrap()
        .remove("provider_sequence");
    assert!(
        call_provider("resource_plan", "smoke.missing-sequence", &missing_sequence)
            .unwrap_err()
            .starts_with("invalid_request:")
    );
    provider_request.provider_sequence = 0;
    let zero_sequence = call_provider(
        "resource_plan",
        "smoke.zero-sequence",
        &serde_json::to_value(&provider_request).unwrap(),
    )
    .unwrap();
    assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
    assert_eq!(
        lifecycle.take_serialized::<Value>(zero_sequence).unwrap(),
        Some(json!({"status":"rejected","reason":"invalid_request"}))
    );
    provider_request.provider_sequence = 1;
    provider_request.settings = values;
    let plan = call_provider(
        "resource_plan",
        "smoke.plan",
        &serde_json::to_value(&provider_request).unwrap(),
    )
    .unwrap();
    assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
    let result = lifecycle.take_serialized::<Value>(plan).unwrap();
    assert_eq!(
        result,
        Some(json!({"status":"rejected","reason":"disabled_or_invalid_settings"}))
    );
    let status = amaterasu_module_sdk::extension_api::ProviderStatusRequest {
        provider_sequence: 1,
        operation_id: "plan-1".into(),
        transition_id: "transition-1".into(),
        receipt_id: "plan-1".into(),
    };
    let status_id = call_provider(
        "lifecycle_status",
        "smoke.status",
        &serde_json::to_value(status).unwrap(),
    )
    .unwrap();
    let mut status_result = None;
    for _ in 0..8 {
        assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
        status_result = lifecycle.take_serialized::<Value>(status_id).unwrap();
        if status_result.is_some() {
            break;
        }
    }
    assert_eq!(
        status_result,
        Some(json!({"status":"recovery_required","reason":"receipt_lookup_unknown"}))
    );
    // SAFETY: module retires before its library and runtime are destroyed.
    unsafe {
        (api.destroy)(instance);
        destroy_runtime(runtime);
    }
}
