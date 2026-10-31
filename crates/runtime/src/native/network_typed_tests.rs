use amaterasu_abi::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_module_sdk::{RuntimeClient, network::Network};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicUsize, Ordering},
};

unsafe extern "C" fn wrong_factory(
    context: *mut c_void,
    _: BorrowedBytes,
    _: *const NativeInterfaceQuery,
    _: *mut *const NativeInterface,
) -> Status {
    // SAFETY: test retains the atomic through every factory call.
    unsafe { &*context.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    STATUS_INVALID_ARGUMENT
}
#[test]
fn safe_network_selection_rejects_other_contract_alias_without_calling_factory() {
    let mut config = RuntimeConfig::default();
    config.principals.insert(
        "provider".into(),
        Grants {
            native_provide: ["example.alias/1".into()].into(),
            ..Default::default()
        },
    );
    config.principals.insert(
        "consumer".into(),
        Grants {
            native_use: ["example.alias/1".into()].into(),
            ..Default::default()
        },
    );
    // SAFETY: immutable runtime export table has static storage.
    let api = unsafe { &*crate::ffi::amaterasu_runtime_v1() };
    let bytes = serde_json::to_vec(&config).unwrap();
    let mut runtime = std::ptr::null_mut();
    assert_eq!(
        unsafe { (api.create)(BorrowedBytes::new(&bytes), &mut runtime) },
        STATUS_OK
    );
    let endpoint = |name: &str| {
        let mut output = std::mem::MaybeUninit::uninit();
        assert_eq!(
            unsafe {
                (api.endpoint)(
                    runtime,
                    BorrowedBytes::new(name.as_bytes()),
                    output.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        // SAFETY: runtime's endpoint has thread-safe invoke/context; it outlives clients.
        unsafe { RuntimeClient::from_endpoint(output.assume_init()) }
    };
    let provider = endpoint("provider");
    let consumer = endpoint("consumer");
    let calls = AtomicUsize::new(0);
    let factory = NativeServiceFactory {
        header: AbiHeader::of::<NativeServiceFactory>(),
        context: (&calls as *const AtomicUsize).cast_mut().cast(),
        bind: Some(wrong_factory),
    };
    unsafe {
        provider
            .register_native_typed("example.alias/1", "example.other-interface/1", &factory)
            .unwrap();
    }
    let settings = serde_json::json!({"network_provider":"example.alias/1"});
    assert!(Network::from_settings(&consumer, &settings).is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "wrong factory was reached before contract validation"
    );
    // No binding was created and every endpoint call has ended.
    unsafe { (api.destroy)(runtime) };
}
