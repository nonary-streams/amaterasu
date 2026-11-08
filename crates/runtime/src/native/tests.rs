use super::*;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Context {
    services: *const NativeServices,
    binds: AtomicUsize,
    releases: AtomicUsize,
    observed: Mutex<Vec<String>>,
    incompatible: bool,
}
#[repr(C)]
struct Table {
    base: NativeInterface,
    context: *const Context,
}
unsafe extern "C" fn release(context: *mut c_void) {
    // SAFETY: each successful factory allocates this table once; release owns it.
    let table = unsafe { Box::from_raw(context.cast::<Table>()) };
    let context = unsafe { &*table.context };
    context.releases.fetch_add(1, Ordering::SeqCst);
    // A successful try_lock proves release did not run under the registry mutex.
    assert!(unsafe { &*context.services }.registry.try_lock().is_ok());
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    caller: BorrowedBytes,
    _: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: test fixture outlives every synchronous bind and resulting release.
    let context = unsafe { &*context.cast::<Context>() };
    assert!(unsafe { &*context.services }.registry.try_lock().is_ok());
    context.binds.fetch_add(1, Ordering::SeqCst);
    context
        .observed
        .lock()
        .unwrap()
        .push(String::from_utf8(unsafe { caller.read().unwrap() }.to_vec()).unwrap());
    let mut table = Box::new(Table {
        base: NativeInterface {
            header: AbiHeader {
                size: size_of::<Table>() as u32,
                major: if context.incompatible { 2 } else { 1 },
                minor: 0,
            },
            context: std::ptr::null_mut(),
            release: Some(release),
        },
        context,
    });
    table.base.context = (&mut *table as *mut Table).cast();
    unsafe {
        *output = Box::into_raw(table).cast();
    }
    STATUS_OK
}
fn services() -> NativeServices {
    let mut grants = BTreeMap::new();
    grants.insert(
        "provider".into(),
        Grants {
            native_provide: ["frames/1".into()].into(),
            ..Default::default()
        },
    );
    grants.insert(
        "consumer".into(),
        Grants {
            native_use: ["frames/1".into()].into(),
            ..Default::default()
        },
    );
    grants.insert(
        "console".into(),
        Grants {
            call: ["frames/1".into()].into(),
            provide: ["frames/1".into()].into(),
            ..Default::default()
        },
    );
    NativeServices::new(grants, BTreeMap::new())
}
fn query() -> Command {
    Command::NativeBind {
        service: "frames/1".into(),
        interface_major: 1,
        min_minor: 0,
        min_size: size_of::<Table>() as u32,
    }
}
fn context(services: &NativeServices, incompatible: bool) -> Context {
    Context {
        services,
        binds: AtomicUsize::new(0),
        releases: AtomicUsize::new(0),
        observed: Mutex::new(vec![]),
        incompatible,
    }
}
fn factory(context: &Context) -> NativeServiceFactory {
    NativeServiceFactory {
        header: AbiHeader::of::<NativeServiceFactory>(),
        context: (context as *const Context).cast_mut().cast(),
        bind: Some(bind),
    }
}
#[test]
fn ordinary_call_grants_never_authorize_pointer_registration_or_binding() {
    let services = services();
    // Address 1 would be invalid to dereference; permission rejection must come first.
    let response = unsafe {
        services.dispatch(
            "console",
            Command::NativeRegister {
                service: "frames/1".into(),
                factory: 1,
            },
        )
    };
    assert!(matches!(response, Response::Error { code,.. } if code=="permission_denied"));
    assert!(
        matches!(unsafe {services.dispatch("console",query())}, Response::Error {code,..} if code=="permission_denied")
    );
}
#[test]
fn authenticated_principal_release_ownership_and_callbacks_without_registry_lock() {
    let services = services();
    let context = context(&services, false);
    let factory = factory(&context);
    assert!(matches!(
        unsafe {
            services.dispatch(
                "provider",
                Command::NativeRegister {
                    service: "frames/1".into(),
                    factory: &factory as *const _ as usize,
                },
            )
        },
        Response::Ok
    ));
    let Response::NativeBound { binding, .. } = (unsafe { services.dispatch("consumer", query()) })
    else {
        panic!("bind failed")
    };
    assert_eq!(*context.observed.lock().unwrap(), vec!["consumer"]);
    assert!(
        matches!(unsafe {services.dispatch("console",Command::NativeRelease {binding})},Response::Error {code,..} if code=="permission_denied")
    );
    assert_eq!(context.releases.load(Ordering::SeqCst), 0);
    assert!(matches!(
        unsafe { services.dispatch("consumer", Command::NativeRelease { binding }) },
        Response::Ok
    ));
    assert!(
        unsafe { services.dispatch("consumer", Command::NativeRelease { binding }) }.is_error()
    );
    assert_eq!(context.releases.load(Ordering::SeqCst), 1);
}
#[test]
fn incompatible_interface_is_released_and_reservation_recovered() {
    let services = services();
    let context = context(&services, true);
    let factory = factory(&context);
    unsafe {
        services.dispatch(
            "provider",
            Command::NativeRegister {
                service: "frames/1".into(),
                factory: &factory as *const _ as usize,
            },
        )
    };
    assert!(unsafe { services.dispatch("consumer", query()) }.is_error());
    assert_eq!(context.releases.load(Ordering::SeqCst), 1);
    assert!(services.registry.lock().unwrap().bindings.is_empty());
}

unsafe extern "C" fn release_after_poison(context: *mut c_void) {
    // SAFETY: the factory transfers this unique table to the runtime.
    let table = unsafe { Box::from_raw(context.cast::<Table>()) };
    let context = unsafe { &*table.context };
    // A poisoned but unlocked mutex proves the callback is outside the lock.
    assert!(matches!(
        unsafe { &*context.services }.registry.try_lock(),
        Err(std::sync::TryLockError::Poisoned(_))
    ));
    context.releases.fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn bind_then_poison_registry(
    context: *mut c_void,
    caller: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: this fixture uses the same live context and output as `bind`.
    let status = unsafe { bind(context, caller, query, output) };
    let table = unsafe { (*output).cast_mut().cast::<Table>() };
    unsafe { (*table).base.release = Some(release_after_poison) };
    let context = unsafe { &*context.cast::<Context>() };
    let services = unsafe { &*context.services };
    let _ = std::panic::catch_unwind(|| {
        let _guard = services.registry.lock().unwrap();
        panic!("injected registry failure during bind");
    });
    status
}

#[test]
fn poisoned_registry_releases_incompatible_factory_output() {
    let services = services();
    let context = context(&services, true);
    let mut factory = factory(&context);
    factory.bind = Some(bind_then_poison_registry);
    assert!(matches!(
        unsafe {
            services.dispatch(
                "provider",
                Command::NativeRegister {
                    service: "frames/1".into(),
                    factory: &factory as *const _ as usize,
                },
            )
        },
        Response::Ok
    ));
    assert!(matches!(
        unsafe { services.dispatch("consumer", query()) },
        Response::Error { code, .. } if code == "runtime_unavailable"
    ));
    assert_eq!(context.releases.load(Ordering::SeqCst), 1);
}
#[test]
fn short_factory_header_is_rejected_before_callback_read() {
    let services = services();
    let header = AbiHeader::of::<AbiHeader>();
    assert!(
        unsafe {
            services.dispatch(
                "provider",
                Command::NativeRegister {
                    service: "frames/1".into(),
                    factory: &header as *const _ as usize,
                },
            )
        }
        .is_error()
    );
}

#[test]
fn factory_descriptor_can_move_after_registration() {
    let services = services();
    let context = context(&services, false);
    let mut descriptor = factory(&context);
    // SAFETY: descriptor and context are live during registration; only context
    // and callback code are retained after this synchronous call.
    assert!(matches!(
        unsafe {
            services.dispatch(
                "provider",
                Command::NativeRegister {
                    service: "frames/1".into(),
                    factory: (&descriptor as *const NativeServiceFactory) as usize,
                },
            )
        },
        Response::Ok
    ));
    descriptor.context = std::ptr::null_mut();
    descriptor.bind = None;
    assert!(descriptor.bind.is_none());
    // SAFETY: the original context/code still outlive the binding. The overwritten
    // descriptor is deliberately unusable and must not be consulted again.
    let Response::NativeBound { binding, .. } = (unsafe { services.dispatch("consumer", query()) })
    else {
        panic!("copied factory did not bind");
    };
    assert!(matches!(
        unsafe { services.dispatch("consumer", Command::NativeRelease { binding }) },
        Response::Ok
    ));
    assert_eq!(context.binds.load(Ordering::SeqCst), 1);
    assert_eq!(context.releases.load(Ordering::SeqCst), 1);
}

fn typed_query(contract: &str) -> Command {
    Command::NativeBindTyped {
        service: "frames/1".into(),
        contract: contract.into(),
        interface_major: 1,
        min_minor: 0,
        min_size: size_of::<Table>() as u32,
    }
}

#[test]
fn typed_native_binding_rejects_wrong_family_before_any_factory_callback() {
    let services = services();
    let context = context(&services, false);
    let factory = factory(&context);
    // SAFETY: factory/context outlive every synchronous call and resulting table.
    assert!(matches!(
        unsafe {
            services.dispatch(
                "provider",
                Command::NativeRegisterTyped {
                    service: "frames/1".into(),
                    factory: &factory as *const _ as usize,
                    contract: "example.frames-interface/1".into(),
                },
            )
        },
        Response::Ok
    ));
    // Both families advertise the same major and minimum size. Their semantic
    // table identity must still be checked before invoking either factory.
    assert!(
        matches!(unsafe { services.dispatch("consumer", typed_query("example.other-interface/1")) },
        Response::Error { code, .. } if code == "native_contract_mismatch")
    );
    assert_eq!(context.binds.load(Ordering::SeqCst), 0);
    assert!(services.registry.lock().unwrap().bindings.is_empty());
    let Response::NativeBound { binding, .. } =
        (unsafe { services.dispatch("consumer", typed_query("example.frames-interface/1")) })
    else {
        panic!("matching contract rejected");
    };
    assert_eq!(context.binds.load(Ordering::SeqCst), 1);
    assert!(matches!(
        unsafe { services.dispatch("consumer", Command::NativeRelease { binding }) },
        Response::Ok
    ));
    assert_eq!(context.releases.load(Ordering::SeqCst), 1);
}

#[test]
fn typed_binding_never_infers_contract_from_an_untyped_legacy_factory() {
    let services = services();
    let context = context(&services, false);
    let factory = factory(&context);
    // SAFETY: test keeps factory/context live until the runtime calls finish.
    assert!(matches!(
        unsafe {
            services.dispatch(
                "provider",
                Command::NativeRegister {
                    service: "frames/1".into(),
                    factory: &factory as *const _ as usize,
                },
            )
        },
        Response::Ok
    ));
    assert!(
        matches!(unsafe { services.dispatch("consumer", typed_query("example.frames-interface/1")) },
        Response::Error { code, .. } if code == "native_contract_mismatch")
    );
    assert_eq!(context.binds.load(Ordering::SeqCst), 0);
    // Legacy explicitly unsafe consumers keep their original compatibility path.
    let Response::NativeBound { binding, .. } = (unsafe { services.dispatch("consumer", query()) })
    else {
        panic!("legacy bind rejected");
    };
    unsafe {
        services.dispatch("consumer", Command::NativeRelease { binding });
    }
}

#[test]
fn legacy_contract_approval_is_scoped_and_does_not_override_typed_registration() {
    for (principal, service, explicit, accepted) in [
        ("provider", "frames/1", false, true),
        ("other", "frames/1", false, false),
        ("provider", "other/1", false, false),
        ("provider", "frames/1", true, false),
    ] {
        let grants = BTreeMap::from([
            (
                "provider".into(),
                Grants {
                    native_provide: ["frames/1".into(), "other/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "other".into(),
                Grants {
                    native_provide: ["frames/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "consumer".into(),
                Grants {
                    native_use: ["frames/1".into(), "other/1".into()].into(),
                    ..Default::default()
                },
            ),
        ]);
        let approvals = BTreeMap::from([(
            "provider".into(),
            BTreeMap::from([("frames/1".into(), "example.frames/1".into())]),
        )]);
        let services = NativeServices::new(grants, approvals);
        let context = context(&services, false);
        let descriptor = factory(&context);
        let address = &descriptor as *const _ as usize;
        let command = if explicit {
            Command::NativeRegisterTyped {
                service: service.into(),
                factory: address,
                contract: "example.other/1".into(),
            }
        } else {
            Command::NativeRegister {
                service: service.into(),
                factory: address,
            }
        };
        assert!(matches!(
            unsafe { services.dispatch(principal, command) },
            Response::Ok
        ));
        let query = |contract: &str| Command::NativeBindTyped {
            service: service.into(),
            contract: contract.into(),
            interface_major: 1,
            min_minor: 0,
            min_size: size_of::<Table>() as u32,
        };
        assert!(
            matches!(unsafe { services.dispatch("consumer", query("example.wrong/1")) }, Response::Error { code, .. } if code == "native_contract_mismatch")
        );
        assert_eq!(context.binds.load(Ordering::SeqCst), 0);
        let response = unsafe { services.dispatch("consumer", query("example.frames/1")) };
        if accepted {
            let Response::NativeBound { binding, .. } = response else {
                panic!("approved binding failed")
            };
            assert!(matches!(
                unsafe { services.dispatch("consumer", Command::NativeRelease { binding }) },
                Response::Ok
            ));
        } else {
            assert!(
                matches!(response, Response::Error { code, .. } if code == "native_contract_mismatch")
            );
            assert_eq!(context.binds.load(Ordering::SeqCst), 0);
        }
        if explicit {
            let Response::NativeBound { binding, .. } =
                (unsafe { services.dispatch("consumer", query("example.other/1")) })
            else {
                panic!("explicit family was overridden")
            };
            assert!(matches!(
                unsafe { services.dispatch("consumer", Command::NativeRelease { binding }) },
                Response::Ok
            ));
        }
    }
}
