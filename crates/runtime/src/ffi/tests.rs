use super::*;
mod display_plugin_smoke;
use amaterasu_contracts::Grants;
use amaterasu_module_sdk::RuntimeClient;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    endpoint: HostApi,
    observed: Mutex<Vec<String>>,
    releases: AtomicUsize,
}
#[repr(C)]
struct Interface {
    base: NativeInterface,
    fixture: *const Fixture,
    value: u64,
}
unsafe extern "C" fn release(context: *mut c_void) {
    // SAFETY: exactly one runtime release owns the allocated interface.
    let table = unsafe { Box::from_raw(context.cast::<Interface>()) };
    let fixture = unsafe { &*table.fixture };
    fixture.releases.fetch_add(1, Ordering::SeqCst);
    let endpoint = unsafe { &*fixture.endpoint.context.cast::<Endpoint>() };
    assert!(endpoint.broker.try_lock().is_ok());
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: fixture, query and principal borrow remain live for this callback.
    let fixture = unsafe { &*context.cast::<Fixture>() };
    assert!(unsafe { (*query).header.supports::<NativeInterfaceQuery>() });
    let endpoint = unsafe { &*fixture.endpoint.context.cast::<Endpoint>() };
    assert!(endpoint.broker.try_lock().is_ok());
    // Exercise actual reentrant JSON transport after proving the broker is unlocked.
    let reply = unsafe { fixture.endpoint.call(br#"{"op":"get_settings"}"#) }.unwrap();
    assert!(matches!(
        serde_json::from_slice::<Response>(&reply).unwrap(),
        Response::Result { .. }
    ));
    fixture.observed.lock().unwrap().push(
        std::str::from_utf8(unsafe { caller.read().unwrap() })
            .unwrap()
            .into(),
    );
    let mut table = Box::new(Interface {
        base: NativeInterface {
            header: AbiHeader::of::<Interface>(),
            context: std::ptr::null_mut(),
            release: Some(release),
        },
        fixture,
        value: 42,
    });
    table.base.context = (&mut *table as *mut Interface).cast();
    unsafe {
        *output = Box::into_raw(table).cast();
    }
    STATUS_OK
}
unsafe fn endpoint(runtime: *mut c_void, principal: &str) -> HostApi {
    let mut output = std::mem::MaybeUninit::<HostApi>::uninit();
    assert_eq!(
        unsafe {
            create_endpoint(
                runtime,
                BorrowedBytes::new(principal.as_bytes()),
                output.as_mut_ptr(),
            )
        },
        STATUS_OK
    );
    unsafe { output.assume_init() }
}
#[test]
fn ffi_sdk_binding_delivers_table_and_releases_without_holding_broker_lock() {
    let configuration = RuntimeConfig {
        principals: [
            (
                "provider".into(),
                Grants {
                    native_provide: ["example/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "consumer".into(),
                Grants {
                    native_use: ["example/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "console".into(),
                Grants {
                    call: ["example/1".into()].into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let bytes = serde_json::to_vec(&configuration).unwrap();
    let mut runtime = std::ptr::null_mut();
    assert_eq!(
        unsafe { create_runtime(BorrowedBytes::new(&bytes), &mut runtime) },
        STATUS_OK
    );
    {
        let provider = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "provider")) };
        let consumer = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "consumer")) };
        let console = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "console")) };
        let fixture = Fixture {
            endpoint: unsafe { endpoint(runtime, "provider") },
            observed: Mutex::new(vec![]),
            releases: AtomicUsize::new(0),
        };
        let factory = NativeServiceFactory {
            header: AbiHeader::of::<NativeServiceFactory>(),
            context: (&fixture as *const Fixture).cast_mut().cast(),
            bind: Some(bind),
        };
        unsafe { provider.register_native("example/1", &factory) }.unwrap();
        assert!(
            provider
                .send(Command::NativeRegister {
                    service: "example/1".into(),
                    factory: 1
                })
                .is_err()
        );
        assert!(
            unsafe { console.bind_native("example/1", 1, 0, size_of::<Interface>() as u32) }
                .is_err()
        );
        let binding =
            unsafe { consumer.bind_native("example/1", 1, 0, size_of::<Interface>() as u32) }
                .unwrap();
        assert_eq!(unsafe { binding.table::<Interface>() }.unwrap().value, 42);
        assert_eq!(*fixture.observed.lock().unwrap(), vec!["consumer"]);
        drop(binding);
        assert_eq!(fixture.releases.load(Ordering::SeqCst), 1);
        // More than the live registry capacity over time must work when bindings drop.
        for _ in 0..1100 {
            let binding =
                unsafe { consumer.bind_native("example/1", 1, 0, size_of::<Interface>() as u32) }
                    .unwrap();
            drop(binding);
        }
        assert_eq!(fixture.releases.load(Ordering::SeqCst), 1101);
    }
    unsafe { destroy_runtime(runtime) };
}
