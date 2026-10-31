//! Opt-in test of independently built library and discovery provider modules.
use super::*;
use amaterasu_module_sdk::requests::RequestOptions;
use serde_json::{Value, json};
use std::path::PathBuf;

const LIBRARY: &str = "amaterasu.library/1";
const PROVIDER: &str = "example.library.provider/1";

#[test]
#[ignore = "requires independently built library and provider artifacts"]
fn discovery_round_trip_through_loaded_modules_and_broker() {
    let configuration = RuntimeConfig {
        principals: [
            ("amaterasu.library".into(), Grants { provide: [LIBRARY.into()].into(), call: [PROVIDER.into()].into(), ..Default::default() }),
            ("example.provider".into(), Grants { provide: [PROVIDER.into()].into(), ..Default::default() }),
            ("consumer".into(), Grants { call: [LIBRARY.into(), PROVIDER.into()].into(), ..Default::default() }),
            ("ungranted".into(), Grants { call: [LIBRARY.into()].into(), ..Default::default() }),
            ("unauthorized".into(), Grants { call: [LIBRARY.into(), PROVIDER.into()].into(), ..Default::default() }),
            ("adapter-denied".into(), Grants { call: [LIBRARY.into(), PROVIDER.into()].into(), ..Default::default() }),
        ].into(),
        settings: [
            ("amaterasu.library".into(), json!({"discovery":{"providers":{"fixture":{"service":PROVIDER,"allowed_initiators":["consumer","ungranted","adapter-denied"]}}}})),
            ("example.provider".into(), json!({"library_caller":"amaterasu.library","allowed_initiators":["consumer","ungranted"]})),
        ].into(),
        ..Default::default()
    };
    let bytes = serde_json::to_vec(&configuration).unwrap();
    let mut runtime = std::ptr::null_mut();
    // SAFETY: input and output remain valid throughout synchronous runtime creation.
    assert_eq!(
        unsafe { create_runtime(BorrowedBytes::new(&bytes), &mut runtime) },
        STATUS_OK
    );
    // SAFETY: supplied artifacts are trusted test modules and remain loaded until destruction.
    let library = unsafe {
        libloading::Library::new(PathBuf::from(
            std::env::var_os("AMATERASU_LIBRARY_PLUGIN").expect("library path"),
        ))
    }
    .unwrap();
    let provider = unsafe {
        libloading::Library::new(PathBuf::from(
            std::env::var_os("AMATERASU_PROVIDER_PLUGIN").expect("provider path"),
        ))
    }
    .unwrap();
    // SAFETY: both modules export the documented entry point and their handles outlive instances.
    let library_entry: libloading::Symbol<ModuleEntry> =
        unsafe { library.get(b"amaterasu_module_v1\0") }.unwrap();
    let provider_entry: libloading::Symbol<ModuleEntry> =
        unsafe { provider.get(b"amaterasu_module_v1\0") }.unwrap();
    let library_api = unsafe { &*library_entry() };
    let provider_api = unsafe { &*provider_entry() };
    assert!(
        library_api.header.supports::<ModuleApi>() && provider_api.header.supports::<ModuleApi>()
    );
    let mut library_instance = std::ptr::null_mut();
    let mut provider_instance = std::ptr::null_mut();
    // SAFETY: endpoints and output slots are valid; runtime and modules outlive the instances.
    assert_eq!(
        unsafe {
            (library_api.create)(
                &endpoint(runtime, "amaterasu.library"),
                &mut library_instance,
            )
        },
        STATUS_OK
    );
    assert_eq!(
        unsafe {
            (provider_api.create)(
                &endpoint(runtime, "example.provider"),
                &mut provider_instance,
            )
        },
        STATUS_OK
    );
    assert!(!library_instance.is_null() && !provider_instance.is_null());
    let invoke = |principal: &str, operation: &str| {
        // SAFETY: runtime outlives each synchronous client call.
        let client = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, principal)) };
        let id = client
            .request_serialized(
                RequestOptions {
                    service: LIBRARY,
                    timeout_ms: 5_000,
                    operation_id: operation,
                    parent: None,
                },
                &json!({"action":"discover","provider":"fixture","cursor":null,"limit":20}),
            )
            .unwrap();
        for _ in 0..8 {
            // SAFETY: both instances are live and called serially.
            assert_eq!(unsafe { (library_api.tick)(library_instance) }, STATUS_OK);
            assert_eq!(unsafe { (provider_api.tick)(provider_instance) }, STATUS_OK);
            if let Some(result) = client.take_serialized::<Value>(id).unwrap() {
                return result;
            }
        }
        panic!("discovery did not complete")
    };
    assert_eq!(
        invoke("consumer", "success"),
        json!({"ok":true,"revision":"fixture-v1","entries":[{"id":"fixture-game","name":"Fixture Game","artwork":{"id":"fixture-cover","revision":"fixture-v1"}}],"next_cursor":null})
    );
    assert_eq!(
        invoke("ungranted", "missing-grant")["error"]["code"],
        "provider_unavailable"
    );
    assert_eq!(
        invoke("unauthorized", "unauthorized")["error"]["code"],
        "forbidden"
    );
    assert_eq!(
        invoke("adapter-denied", "adapter-denied")["error"]["code"],
        "unauthorized"
    );
    // SAFETY: instances retire before their loaded libraries and the owning runtime.
    unsafe {
        (library_api.destroy)(library_instance);
        (provider_api.destroy)(provider_instance);
        destroy_runtime(runtime);
    }
}
