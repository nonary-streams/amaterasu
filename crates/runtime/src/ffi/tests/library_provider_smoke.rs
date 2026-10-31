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

#[test]
#[ignore = "requires independently built integrated library artifact"]
fn playnite_transfer_release_retries_through_broker() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    const APP: &str = "app-0000000000000001";
    const GAME: &str = "11111111-2222-3333-4444-555555555555";
    const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let config = RuntimeConfig {
        principals: [
            (
                "amaterasu.nvhttp".into(),
                Grants {
                    call: [
                        LIBRARY.into(),
                        "amaterasu.catalog/1".into(),
                        "amaterasu.playnite/1".into(),
                    ]
                    .into(),
                    ..Default::default()
                },
            ),
            (
                "amaterasu.library".into(),
                Grants {
                    provide: [LIBRARY.into()].into(),
                    call: ["amaterasu.catalog/1".into(), "amaterasu.playnite/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "amaterasu.catalog".into(),
                Grants {
                    provide: ["amaterasu.catalog/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "amaterasu.playnite".into(),
                Grants {
                    provide: ["amaterasu.playnite/1".into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let bytes = serde_json::to_vec(&config).unwrap();
    let mut runtime = std::ptr::null_mut();
    // SAFETY: config bytes and output remain valid for runtime creation.
    assert_eq!(
        unsafe { create_runtime(BorrowedBytes::new(&bytes), &mut runtime) },
        STATUS_OK
    );
    // SAFETY: runtime remains live for all endpoint clients.
    let consumer = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "amaterasu.nvhttp")) };
    let catalog = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "amaterasu.catalog")) };
    let provider = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "amaterasu.playnite")) };
    for (client, service) in [
        (&catalog, "amaterasu.catalog/1"),
        (&provider, "amaterasu.playnite/1"),
    ] {
        client
            .require_success(Command::Register {
                service: service.into(),
            })
            .unwrap();
    }
    // SAFETY: the supplied trusted module remains loaded until instance destruction.
    let library = unsafe {
        libloading::Library::new(PathBuf::from(
            std::env::var_os("AMATERASU_LIBRARY_PLUGIN").expect("library path"),
        ))
    }
    .unwrap();
    // SAFETY: fixture exports the documented module ABI.
    let entry: libloading::Symbol<ModuleEntry> =
        unsafe { library.get(b"amaterasu_module_v1\0") }.unwrap();
    let api = unsafe { &*entry() };
    assert!(api.header.supports::<ModuleApi>());
    let mut instance = std::ptr::null_mut();
    // SAFETY: the endpoint and output are valid for synchronous creation.
    assert_eq!(
        unsafe { (api.create)(&endpoint(runtime, "amaterasu.library"), &mut instance) },
        STATUS_OK
    );
    let tick = || {
        // SAFETY: instance remains live and is invoked serially.
        assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
    };
    let send = |operation: &str, payload: Value| {
        consumer
            .request_serialized(
                RequestOptions {
                    service: LIBRARY,
                    timeout_ms: 5_000,
                    operation_id: operation,
                    parent: None,
                },
                &payload,
            )
            .unwrap()
    };
    let read = send(
        "read",
        json!({"action":"artwork_read","id":APP,"revision":null,"offset":0,"transfer_id":TOKEN}),
    );
    for _ in 0..8 {
        tick();
        if let Some(request) = catalog.poll_serialized().unwrap() {
            assert_eq!(request.caller, "amaterasu.library");
            assert_eq!(request.initiator, "amaterasu.nvhttp");
            catalog.complete_serialized(request.id, &json!({"ok":true,"artwork":{"provider":"playnite","game_id":GAME,"revision":REVISION}})).unwrap();
            break;
        }
    }
    let mut served = false;
    for _ in 0..8 {
        tick();
        if let Some(request) = provider.poll_serialized().unwrap() {
            assert_eq!(request.caller, "amaterasu.library");
            assert_eq!(request.initiator, "amaterasu.nvhttp");
            assert_eq!(request.payload["transfer_id"], TOKEN);
            provider.complete_serialized(request.id, &json!({"ok":true,"game_id":GAME,"revision":REVISION,"transfer_id":TOKEN,"offset":0,"next_offset":null,"total_bytes":3,"mime_type":"image/png","data":"YWJj"})).unwrap();
            served = true;
            break;
        }
    }
    assert!(served, "provider did not receive read");
    let mut read_done = false;
    for _ in 0..8 {
        tick();
        if let Some(result) = consumer.take_serialized::<Value>(read).unwrap() {
            assert_eq!(result["ok"], true, "{result}");
            read_done = true;
            break;
        }
    }
    assert!(read_done);
    let release = send(
        "release-fail",
        json!({"action":"artwork_release","id":APP,"transfer_id":TOKEN}),
    );
    let mut forwarded = false;
    for _ in 0..8 {
        tick();
        if let Some(request) = provider.poll_serialized().unwrap() {
            assert_eq!(request.initiator, "amaterasu.nvhttp");
            assert_eq!(
                request.payload,
                json!({"action":"artwork_release","transfer_id":TOKEN})
            );
            provider
                .complete_serialized(
                    request.id,
                    &json!({"ok":true,"transfer_id":"ffffffffffffffffffffffffffffffff"}),
                )
                .unwrap();
            forwarded = true;
            break;
        }
    }
    assert!(forwarded);
    let mut failed = false;
    for _ in 0..8 {
        tick();
        if let Some(result) = consumer.take_serialized::<Value>(release).unwrap() {
            assert_eq!(result["ok"], false, "{result}");
            failed = true;
            break;
        }
    }
    assert!(failed);
    let mut cleanup_seen = false;
    for _ in 0..8 {
        tick();
        if let Some(request) = provider.poll_serialized().unwrap() {
            assert_eq!(request.caller, "amaterasu.library");
            assert_eq!(request.initiator, "amaterasu.library");
            assert_eq!(
                request.payload,
                json!({"action":"artwork_release","transfer_id":TOKEN})
            );
            provider
                .complete_serialized(request.id, &json!({"ok":true,"transfer_id":TOKEN}))
                .unwrap();
            cleanup_seen = true;
            break;
        }
    }
    assert!(cleanup_seen, "detached cleanup did not reach provider");
    // Allow the library to consume the exact cleanup acknowledgment before retrying.
    for _ in 0..3 {
        tick();
    }
    let duplicate = send(
        "release-duplicate",
        json!({"action":"artwork_release","id":APP,"transfer_id":TOKEN}),
    );
    let mut duplicate_done = false;
    for _ in 0..8 {
        tick();
        assert!(provider.poll_serialized().unwrap().is_none());
        if let Some(result) = consumer.take_serialized::<Value>(duplicate).unwrap() {
            assert_eq!(result, json!({"ok":true}));
            duplicate_done = true;
            break;
        }
    }
    assert!(duplicate_done);
    // SAFETY: module instance retires before library and runtime are destroyed.
    unsafe {
        (api.destroy)(instance);
        destroy_runtime(runtime);
    }
}
