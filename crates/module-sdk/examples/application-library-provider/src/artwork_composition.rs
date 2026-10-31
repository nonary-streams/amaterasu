//! The same library facade reads an independently compiled provider using opaque
//! IDs and startup-selected SDK routing. No provider-specific facade code exists.
use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ModuleApi, ModuleEntry, ReplySink, STATUS_INTERNAL_ERROR,
    STATUS_OK,
};
use amaterasu_runtime::Broker;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use std::{cell::RefCell, ffi::c_void, rc::Rc};

const LIBRARY: &str = "amaterasu.library/1";
const CATALOG: &str = "amaterasu.catalog/1";
const APP: &str = "app-0000000000000001";
const TRANSFER: &str = "0123456789abcdef0123456789abcdef";
struct Endpoint {
    name: &'static str,
    broker: Rc<RefCell<Broker>>,
}
unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> i32 {
    // SAFETY: harness retains endpoint/input for this synchronous runtime call.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    let Ok(bytes) = (unsafe { input.read() }) else {
        return STATUS_INTERNAL_ERROR;
    };
    let Ok(command) = serde_json::from_slice(bytes) else {
        return STATUS_INTERNAL_ERROR;
    };
    let response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.name, command);
    let output = serde_json::to_vec(&response).unwrap();
    // SAFETY: SDK sink copies its reply before returning.
    unsafe { sink.send(&output) };
    STATUS_OK
}
fn host(endpoint: &mut Endpoint) -> HostApi {
    HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *mut Endpoint).cast(),
        invoke,
    }
}
struct Loaded {
    library: libloading::Library,
    api: *const ModuleApi,
    instance: *mut c_void,
}
impl Loaded {
    unsafe fn new(path: &std::ffi::OsStr, host: &HostApi) -> Self {
        // SAFETY: explicitly supplied trusted fixture; retained until instance retirement.
        let library = unsafe { libloading::Library::new(path) }.unwrap();
        let entry: libloading::Symbol<ModuleEntry> =
            unsafe { library.get(b"amaterasu_module_v1\0") }.unwrap();
        let api = unsafe { entry() };
        let mut instance = std::ptr::null_mut();
        assert_eq!(unsafe { ((*api).create)(host, &mut instance) }, STATUS_OK);
        Self {
            library,
            api,
            instance,
        }
    }
    fn tick(&self) {
        assert_eq!(unsafe { ((*self.api).tick)(self.instance) }, STATUS_OK);
    }
}
impl Drop for Loaded {
    fn drop(&mut self) {
        // SAFETY: instance is live; destroy retires all callbacks before DLL unload.
        unsafe { ((*self.api).destroy)(self.instance) };
        let _ = &self.library;
    }
}

#[test]
#[ignore = "requires independently built facade/provider DLL paths in AMATERASU_LIBRARY_PLUGIN and AMATERASU_PROVIDER_PLUGIN"]
fn third_party_artwork_flows_through_unmodified_facade_and_shared_sdk() {
    for allowed in [true, false] {
        let config = RuntimeConfig { principals: [
            ("library".into(), Grants { provide: [LIBRARY.into()].into(), call: [CATALOG.into(), SERVICE.into()].into(), ..Default::default() }),
            ("provider".into(), Grants { provide: [SERVICE.into()].into(), ..Default::default() }),
            ("catalog".into(), Grants { provide: [CATALOG.into()].into(), ..Default::default() }),
            ("consumer".into(), Grants { call: [LIBRARY.into(), CATALOG.into(), SERVICE.into()].into(), ..Default::default() }),
        ].into(), settings: [
            ("library".into(), json!({"artwork":{"providers":{"vendor":{"service":SERVICE,"allowed_initiators":["consumer"]}}}})),
            ("provider".into(), json!({"callers":{"library":{"initiators":["consumer"],"artwork":allowed}}})),
        ].into(), ..Default::default() };
        let broker = Rc::new(RefCell::new(Broker::new(config).unwrap()));
        assert!(matches!(
            broker.borrow_mut().dispatch(
                "catalog",
                Command::Register {
                    service: CATALOG.into()
                }
            ),
            Response::Ok
        ));
        let mut library_endpoint = Endpoint {
            name: "library",
            broker: broker.clone(),
        };
        let mut provider_endpoint = Endpoint {
            name: "provider",
            broker: broker.clone(),
        };
        let mut consumer_endpoint = Endpoint {
            name: "consumer",
            broker: broker.clone(),
        };
        let library_host = host(&mut library_endpoint);
        let provider_host = host(&mut provider_endpoint);
        let consumer = unsafe { RuntimeClient::from_endpoint(host(&mut consumer_endpoint)) };
        let library_path = std::env::var_os("AMATERASU_LIBRARY_PLUGIN").expect("library path");
        let provider_path = std::env::var_os("AMATERASU_PROVIDER_PLUGIN").expect("provider path");
        let provider = unsafe { Loaded::new(&provider_path, &provider_host) };
        let library = unsafe { Loaded::new(&library_path, &library_host) };
        let Response::Accepted { id } = consumer.send(Command::Request { service: LIBRARY.into(),
            payload: json!({"action":"artwork_read","id":APP,"revision":null,"offset":0,"transfer_id":TRANSFER}),
            timeout_ms: 30_000, operation_id: "opaque-artwork".into(), parent: None }).unwrap() else { panic!("request rejected") };
        let mut completed = None;
        for _ in 0..30 {
            library.tick();
            let catalog_request = broker.borrow_mut().dispatch("catalog", Command::Poll {});
            if let Response::Request { id, .. } = catalog_request {
                let response = json!({"ok":true,"artwork":{"provider":"external","source":"vendor","reference":{"id":"fixture-cover","revision":"fixture-v1"}}});
                assert!(matches!(
                    broker.borrow_mut().dispatch(
                        "catalog",
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
            // A real provider DLL handles its own discovery/capability/image work.
            provider.tick();
            if let Response::Result { payload } =
                consumer.send(Command::TakeResult { request: id }).unwrap()
            {
                completed = Some(payload);
                break;
            }
        }
        let response = completed.expect("artwork operation completed");
        if allowed {
            assert_eq!(response["ok"], true, "{response}");
            assert_eq!(response["revision"], "fixture-v1");
            assert_eq!(response["total_bytes"], COVER.len());
            assert_eq!(response["mime_type"], "image/png");
            assert_eq!(
                STANDARD.decode(response["data"].as_str().unwrap()).unwrap(),
                COVER
            );
            assert!(response.get("reference").is_none());
        } else {
            assert_eq!(
                response["error"]["code"], "artwork_unsupported",
                "{response}"
            );
        }
        consumer
            .require_success(Command::ReleaseResult { request: id })
            .unwrap();
        drop(library);
        drop(provider);
    }
}
