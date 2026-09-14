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
    NativeServices::new(grants)
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
