//! Broker and real Lutris handler, with an independently loaded library facade.
use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ModuleApi, ModuleEntry, ReplySink, STATUS_OK,
};
use amaterasu_runtime::Broker;
use std::{cell::RefCell, ffi::c_void};

struct Endpoint<'a> {
    broker: &'a RefCell<Broker>,
    principal: &'static str,
}
unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> i32 {
    // SAFETY: endpoint and request bytes outlive this synchronous ABI call.
    let endpoint = unsafe { &*context.cast::<Endpoint<'_>>() };
    let command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.principal, command);
    let bytes = serde_json::to_vec(&response).unwrap();
    // SAFETY: sink is supplied by the SDK and valid for this invocation.
    unsafe { sink.send(&bytes) };
    STATUS_OK
}
fn host(endpoint: &Endpoint<'_>) -> HostApi {
    HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *const Endpoint<'_>).cast_mut().cast(),
        invoke,
    }
}
fn accepted(response: Response) -> u64 {
    match response {
        Response::Accepted { id } => id,
        other => panic!("{other:?}"),
    }
}
fn request(service: &str, payload: Value, operation: &str, parent: Option<u64>) -> Command {
    Command::Request {
        service: service.into(),
        payload,
        timeout_ms: 30_000,
        operation_id: operation.into(),
        parent,
    }
}

#[test]
#[ignore = "requires independently built integrated library artifact"]
fn broker_delegates_artwork_to_real_lutris_handler_and_denies_direct_nvhttp() {
    let config = RuntimeConfig {
        principals: [
            (
                "amaterasu.nvhttp".into(),
                Grants {
                    call: [
                        "amaterasu.library/1".into(),
                        "amaterasu.catalog/1".into(),
                        SERVICE.into(),
                    ]
                    .into(),
                    ..Default::default()
                },
            ),
            (
                "amaterasu.library".into(),
                Grants {
                    provide: ["amaterasu.library/1".into()].into(),
                    call: ["amaterasu.catalog/1".into(), SERVICE.into()].into(),
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
                "amaterasu.lutris".into(),
                Grants {
                    provide: [SERVICE.into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let broker = RefCell::new(Broker::new(config).unwrap());
    let lutris_endpoint = Endpoint {
        broker: &broker,
        principal: "amaterasu.lutris",
    };
    let library_endpoint = Endpoint {
        broker: &broker,
        principal: "amaterasu.library",
    };
    // SAFETY: endpoint remains live until the module and its runtime are dropped.
    let mut lutris =
        LutrisModule::start(unsafe { RuntimeClient::from_endpoint(host(&lutris_endpoint)) })
            .unwrap();
    assert!(matches!(
        broker.borrow_mut().dispatch(
            "amaterasu.catalog",
            Command::Register {
                service: "amaterasu.catalog/1".into()
            }
        ),
        Response::Ok
    ));
    let root = std::env::temp_dir().join(format!(
        "amaterasu-broker-artwork-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cover = root.join("coverart/lutris-game-10.png");
    std::fs::create_dir_all(cover.parent().unwrap()).unwrap();
    std::fs::write(
        &cover,
        [b"\x89PNG\r\n\x1a\n".as_slice(), b"fixture"].concat(),
    )
    .unwrap();
    let mut source = snapshot();
    source.database_path = root.join("pga.db");
    lutris.artwork.update(&source);
    let revision = lutris.artwork.metadata(10).unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    // SAFETY: path names a trusted test module and library outlives the instance.
    let library = unsafe {
        libloading::Library::new(
            std::env::var_os("AMATERASU_LIBRARY_PLUGIN").expect("library path"),
        )
    }
    .unwrap();
    // SAFETY: fixture uses the documented module ABI.
    let entry: libloading::Symbol<ModuleEntry> =
        unsafe { library.get(b"amaterasu_module_v1\0") }.unwrap();
    let api = unsafe { &*entry() };
    assert!(api.header.supports::<ModuleApi>());
    let mut instance = std::ptr::null_mut();
    // SAFETY: endpoint and instance output remain valid for this synchronous call.
    assert_eq!(
        unsafe { (api.create)(&host(&library_endpoint), &mut instance) },
        STATUS_OK
    );
    let direct = accepted(broker.borrow_mut().dispatch(
        "amaterasu.nvhttp",
        request(
            SERVICE,
            json!({"action":"artwork_read","game_id":10,"revision":revision,"offset":0}),
            "direct",
            None,
        ),
    ));
    let Response::Request {
        id,
        caller,
        payload,
        ..
    } = broker
        .borrow_mut()
        .dispatch("amaterasu.lutris", Command::Poll {})
    else {
        panic!("direct request not delivered")
    };
    assert_eq!(id, direct);
    lutris.handle_request(id, &caller, payload).unwrap();
    assert!(
        matches!(broker.borrow_mut().dispatch("amaterasu.nvhttp", Command::TakeResult { request: direct }), Response::Result { payload } if payload["code"] == "forbidden")
    );
    let parent = accepted(broker.borrow_mut().dispatch(
        "amaterasu.nvhttp",
        request(
            "amaterasu.library/1",
            json!({"action":"artwork_read","id":"app-0000000000000001","revision":null,"offset":0}),
            "delegated",
            None,
        ),
    ));
    // SAFETY: the module instance is live and called serially.
    assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
    assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
    let Response::Request {
        id,
        caller,
        initiator,
        ..
    } = broker
        .borrow_mut()
        .dispatch("amaterasu.catalog", Command::Poll {})
    else {
        panic!("catalog request not delivered")
    };
    assert_eq!(caller, "amaterasu.library");
    assert_eq!(initiator, "amaterasu.nvhttp");
    assert!(matches!(broker.borrow_mut().dispatch("amaterasu.catalog", Command::Complete { request: id, payload: json!({"ok":true,"artwork":{"provider":"lutris","game_id":10,"revision":revision}}) }), Response::Ok));
    for _ in 0..12 {
        // SAFETY: instance remains live through each synchronous tick.
        assert_eq!(unsafe { (api.tick)(instance) }, STATUS_OK);
        let incoming = broker
            .borrow_mut()
            .dispatch("amaterasu.lutris", Command::Poll {});
        if let Response::Request {
            id,
            caller,
            initiator,
            payload,
            ..
        } = incoming
        {
            assert_eq!(caller, "amaterasu.library");
            assert_eq!(initiator, "amaterasu.nvhttp");
            lutris.handle_request(id, &caller, payload).unwrap();
        }
        if let Response::Result { payload } = broker
            .borrow_mut()
            .dispatch("amaterasu.nvhttp", Command::TakeResult { request: parent })
        {
            assert_eq!(payload["ok"], true, "{payload}");
            assert_eq!(payload["mime_type"], "image/png");
            // SAFETY: module is destroyed before its library, endpoint and broker.
            unsafe { (api.destroy)(instance) };
            std::fs::remove_dir_all(root).unwrap();
            return;
        }
    }
    panic!("artwork request did not complete");
}
