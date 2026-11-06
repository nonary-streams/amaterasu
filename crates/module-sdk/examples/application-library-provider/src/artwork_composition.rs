//! The same library facade reads an independently compiled provider using opaque
//! IDs and startup-selected SDK routing. No provider-specific facade code exists.
use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_module_sdk::abi::{BorrowedBytes, HostApi, ModuleApi, ModuleEntry, STATUS_OK};
use amaterasu_module_sdk::artwork_transfer;
use amaterasu_runtime as _;
use serde_json::json;
use std::ffi::c_void;

const LIBRARY: &str = "amaterasu.library/1";
const CATALOG: &str = "amaterasu.catalog/1";
const APP: &str = "app-0000000000000001";
unsafe extern "C" {
    fn amaterasu_runtime_v1() -> *const amaterasu_module_sdk::abi::RuntimeApi;
}
struct Runtime {
    api: &'static amaterasu_module_sdk::abi::RuntimeApi,
    instance: *mut c_void,
}
impl Runtime {
    fn new(config: RuntimeConfig) -> Self {
        let api = unsafe { &*amaterasu_runtime_v1() };
        let mut instance = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                (api.create)(
                    BorrowedBytes::new(&serde_json::to_vec(&config).unwrap()),
                    &mut instance,
                )
            },
            STATUS_OK
        );
        Self { api, instance }
    }
    fn endpoint(&self, name: &str) -> HostApi {
        let mut host = std::mem::MaybeUninit::uninit();
        assert_eq!(
            unsafe {
                (self.api.endpoint)(
                    self.instance,
                    BorrowedBytes::new(name.as_bytes()),
                    host.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        unsafe { host.assume_init() }
    }
    fn client(&self, name: &str) -> RuntimeClient {
        unsafe { RuntimeClient::from_endpoint(self.endpoint(name)) }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.instance) };
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
            ("library".into(), Grants { provide: [LIBRARY.into()].into(), call: [CATALOG.into(), SERVICE.into()].into(), native_provide: [LIBRARY.into()].into(), native_use: [SERVICE.into()].into(), ..Default::default() }),
            ("provider".into(), Grants { provide: [SERVICE.into()].into(), native_provide: [SERVICE.into()].into(), ..Default::default() }),
            ("catalog".into(), Grants { provide: [CATALOG.into()].into(), ..Default::default() }),
            ("consumer".into(), Grants { call: [LIBRARY.into(), CATALOG.into(), SERVICE.into()].into(), native_use: [LIBRARY.into()].into(), ..Default::default() }),
        ].into(), settings: [
            ("library".into(), json!({"artwork":{"providers":{"vendor":{"service":SERVICE,"allowed_initiators":["consumer"]}}}})),
            ("provider".into(), json!({"callers":{"library":{"initiators":["consumer"],"artwork":allowed}}})),
        ].into(), ..Default::default() };
        let runtime = Runtime::new(config);
        let catalog = runtime.client("catalog");
        catalog
            .require_success(Command::Register {
                service: CATALOG.into(),
            })
            .unwrap();
        let library_host = runtime.endpoint("library");
        let provider_host = runtime.endpoint("provider");
        let consumer = runtime.client("consumer");
        let library_path = std::env::var_os("AMATERASU_LIBRARY_PLUGIN").expect("library path");
        let provider_path = std::env::var_os("AMATERASU_PROVIDER_PLUGIN").expect("provider path");
        let provider = unsafe { Loaded::new(&provider_path, &provider_host) };
        let library = unsafe { Loaded::new(&library_path, &library_host) };
        let Response::Accepted { id } = consumer
            .send(Command::Request {
                service: LIBRARY.into(),
                payload: json!({"action":"artwork_open","id":APP}),
                timeout_ms: 30_000,
                operation_id: "opaque-artwork".into(),
                parent: None,
            })
            .unwrap()
        else {
            panic!("request rejected")
        };
        let mut completed = None;
        for _ in 0..30 {
            library.tick();
            let catalog_request = catalog.send(Command::Poll {}).unwrap();
            if let Response::Request { id, .. } = catalog_request {
                let response = json!({"ok":true,"artwork":{"provider":"external","source":"vendor","reference":{"id":"fixture-cover","revision":"fixture-v1"}}});
                catalog
                    .require_success(Command::Complete {
                        request: id,
                        payload: response,
                    })
                    .unwrap();
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
            let offer = serde_json::from_value(response["offer"].clone()).unwrap();
            let image = unsafe { artwork_transfer::receive(&consumer, LIBRARY, offer) }.unwrap();
            assert_eq!(image.revision, "fixture-v1");
            assert_eq!(image.mime_type, "image/png");
            assert_eq!(image.bytes, COVER);
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
