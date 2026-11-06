//! Exercise the unchanged library against two native provider implementations.
use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_module_sdk::abi::{BorrowedBytes, HostApi, RuntimeApi, STATUS_OK};
use amaterasu_runtime as _;
use std::ffi::c_void;
unsafe extern "C" {
    fn amaterasu_runtime_v1() -> *const RuntimeApi;
}

struct Runtime {
    api: &'static RuntimeApi,
    context: *mut c_void,
}
impl Runtime {
    fn new(service: &str) -> Self {
        let mut config = RuntimeConfig::default();
        for (name, grants) in [
            (
                "library",
                Grants {
                    provide: [SERVICE.into()].into(),
                    native_provide: [SERVICE.into()].into(),
                    call: [CATALOG.into(), service.into()].into(),
                    native_use: [service.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "provider",
                Grants {
                    provide: [service.into()].into(),
                    native_provide: [service.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "catalog",
                Grants {
                    provide: [CATALOG.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "consumer",
                Grants {
                    call: [SERVICE.into(), CATALOG.into(), service.into()].into(),
                    native_use: [SERVICE.into(), service.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "outsider",
                Grants {
                    native_use: [SERVICE.into(), service.into()].into(),
                    ..Default::default()
                },
            ),
        ] {
            config.principals.insert(name.into(), grants);
        }
        config.settings.insert("library".into(), json!({"artwork":{"providers":{"vendor":{"service":service,"allowed_initiators":["consumer"]}}}}));
        // SAFETY: static runtime ABI, valid JSON and writable instance output.
        let api = unsafe { &*amaterasu_runtime_v1() };
        let mut context = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                (api.create)(
                    BorrowedBytes::new(&serde_json::to_vec(&config).unwrap()),
                    &mut context,
                )
            },
            STATUS_OK
        );
        Self { api, context }
    }
    fn client(&self, principal: &str) -> RuntimeClient {
        let mut host = std::mem::MaybeUninit::<HostApi>::uninit();
        // SAFETY: runtime outlives clients and fills the complete endpoint table.
        assert_eq!(
            unsafe {
                (self.api.endpoint)(
                    self.context,
                    BorrowedBytes::new(principal.as_bytes()),
                    host.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        unsafe { RuntimeClient::from_endpoint(host.assume_init()) }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.context) };
    }
}

#[test]
fn interchangeable_native_providers_deliver_large_artwork_without_json_bytes() {
    for service in ["vendor.artwork/1", "replacement.covers/1"] {
        let host = Runtime::new(service);
        let provider = host.client("provider");
        let catalog = host.client("catalog");
        let consumer = host.client("consumer");
        let outsider = host.client("outsider");
        provider
            .require_success(Command::Register {
                service: service.into(),
            })
            .unwrap();
        catalog
            .require_success(Command::Register {
                service: CATALOG.into(),
            })
            .unwrap();
        let mut source = Source::with_chunk_bytes(if service.starts_with("vendor") {
            256 * 1024
        } else {
            64 * 1024
        })
        .unwrap();
        // SAFETY: source is retained beyond the library and every binding.
        unsafe {
            source.register(&provider, service).unwrap();
        }
        let mut library = Library::start(host.client("library")).unwrap();
        let expected = vec![0xab; 1024 * 1024 + 37];
        for attempt in 0..9 {
            let Response::Accepted { id } = consumer
                .send(Command::Request {
                    service: SERVICE.into(),
                    payload: json!({"action":"artwork_open","id":"app-0000000000000001","revision":if attempt == 8 { Some("stale") } else { None }}),
                    timeout_ms: 30_000,
                    operation_id: format!("open.{attempt}"),
                    parent: None,
                })
                .unwrap()
            else {
                panic!()
            };
            let mut response = None;
            for _ in 0..40 {
                library.tick().unwrap();
                if let Response::Request { id, payload, .. } =
                    catalog.send(Command::Poll {}).unwrap()
                {
                    assert_eq!(payload["action"], "resolve_exported_artwork");
                    catalog.require_success(Command::Complete { request: id, payload: json!({"ok":true,"artwork":{"provider":"external","source":"vendor","reference":{"id":"opaque/cover","revision":"rev1"}}}) }).unwrap();
                }
                if let Response::Request {
                    id,
                    caller,
                    payload,
                    ..
                } = provider.send(Command::Poll {}).unwrap()
                {
                    let response = if payload["action"] == "describe" {
                        json!({"ok":true,"capabilities":{"discovery":false,"artwork":true,"lifecycle":false}})
                    } else {
                        assert_eq!(
                            payload,
                            json!({"action":"artwork_open","reference":{"id":"opaque/cover","revision":"rev1"}})
                        );
                        json!({"ok":true,"offer":source.publish(&caller,"rev1".into(),"image/png".into(),expected.clone()).unwrap()})
                    };
                    provider
                        .require_success(Command::Complete {
                            request: id,
                            payload: response,
                        })
                        .unwrap();
                }
                if let Response::Result { payload } =
                    consumer.send(Command::TakeResult { request: id }).unwrap()
                {
                    response = Some(payload);
                    break;
                }
            }
            let response = response.expect("completed");
            if attempt == 8 {
                assert_eq!(response["ok"], false);
                continue;
            }
            assert_eq!(response["ok"], true, "{response}");
            assert!(serde_json::to_vec(&response).unwrap().len() < 1500);
            let offer: Offer = serde_json::from_value(response["offer"].clone()).unwrap();
            assert!(
                unsafe { artwork_transfer::receive(&outsider, SERVICE, offer.clone()) }.is_err()
            );
            let image =
                unsafe { artwork_transfer::receive(&consumer, SERVICE, offer.clone()) }.unwrap();
            assert_eq!(image.bytes, expected);
            assert_eq!(image.revision, "rev1");
            assert!(unsafe { artwork_transfer::receive(&consumer, SERVICE, offer) }.is_err());
            consumer
                .require_success(Command::ReleaseResult { request: id })
                .unwrap();
        }
    }
}

#[test]
fn library_rejects_the_removed_chunk_protocol() {
    assert!(
        serde_json::from_value::<Action>(
            json!({"action":"artwork_read","id":"app-0000000000000001","offset":0})
        )
        .is_err()
    );
}
