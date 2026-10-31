use super::*;
use amaterasu_contracts::{ApiMethodApproval, Command, Grants, RuntimeConfig, SelectedService};
use amaterasu_module_sdk::{
    abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK},
    service::ServiceMethod,
    virtual_display_service::DisplayService,
};
use amaterasu_runtime::Broker;
use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};

struct Endpoint {
    broker: Arc<Mutex<Broker>>,
    principal: &'static str,
}
unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, reply: ReplySink) -> i32 {
    // SAFETY: the endpoint and borrowed request live for the synchronous call.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let result = endpoint
        .broker
        .lock()
        .unwrap()
        .dispatch(endpoint.principal, command);
    let bytes = serde_json::to_vec(&result).unwrap();
    // SAFETY: the sink copies the bytes before this call returns.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}
fn client(endpoint: &Endpoint) -> RuntimeClient {
    let host = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *const Endpoint).cast_mut().cast(),
        invoke,
    };
    // SAFETY: each test keeps endpoint and broker alive through all calls.
    unsafe { RuntimeClient::from_endpoint(host) }
}
fn configuration(
    legacy: bool,
    typed: bool,
    selected: bool,
    approval: Option<bool>,
) -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    let mut provides = std::collections::BTreeSet::new();
    if legacy {
        provides.insert(SERVICE.into());
    }
    if typed {
        provides.insert(display::SERVICE.into());
    }
    config.principals.insert(
        "provider".into(),
        Grants {
            provide: provides,
            ..Default::default()
        },
    );
    config.principals.insert(
        "consumer".into(),
        Grants {
            call: [display::SERVICE.into()].into(),
            ..Default::default()
        },
    );
    if selected {
        config.service_selections.broker.insert(
            display::SERVICE.into(),
            Some(SelectedService {
                provider: "provider".into(),
                contract: "display.service/2".into(),
            }),
        );
    }
    if let Some(exact) = approval {
        let mut method = DisplayService::method();
        if !exact {
            method.request.allow_extra = true;
        }
        config.api_methods.push(ApiMethodApproval {
            provider: "provider".into(),
            callers: ["consumer".into()].into(),
            method,
        });
    }
    config
}
fn starts(config: RuntimeConfig) -> Result<(), String> {
    let broker = Arc::new(Mutex::new(Broker::new(config).unwrap()));
    let endpoint = Endpoint {
        broker,
        principal: "provider",
    };
    let module = VirtualDisplayModule::start(client(&endpoint))?;
    drop(module);
    Ok(())
}

#[test]
fn legacy_only_and_selected_typed_only_compositions_start() {
    assert!(starts(configuration(true, false, false, None)).is_ok());
    assert!(starts(configuration(false, true, true, Some(true))).is_ok());
    assert!(starts(configuration(true, true, false, None)).is_ok());
    assert!(starts(configuration(false, false, false, None)).is_err());
}

#[test]
fn selected_or_approved_incompatible_typed_method_fails_even_with_legacy() {
    for config in [
        configuration(true, true, true, Some(false)),
        configuration(true, true, false, Some(false)),
        configuration(true, true, true, None),
    ] {
        let error = starts(config).unwrap_err();
        assert!(error.contains("incompatible_api"), "{error}");
    }
}
