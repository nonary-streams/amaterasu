//! Two independent trusted providers serve one unchanged SDK consumer.
use super::*;
use amaterasu_contracts::{SelectedService, ServiceSelections};
use amaterasu_module_sdk::identity::{self, IdentityApi, IdentityApiV1_1};

#[repr(C)]
struct Table {
    api: IdentityApiV1_1,
    accepted: bool,
}
unsafe extern "C" fn release(context: *mut c_void) {
    // SAFETY: the successful bind allocates one table and transfers one release.
    drop(unsafe { Box::from_raw(context.cast::<Table>()) });
}
unsafe extern "C" fn bytes(_: *mut c_void, _: ReplySink) -> Status {
    STATUS_OK
}
unsafe extern "C" fn sign(_: *mut c_void, _: u32, _: BorrowedBytes, _: ReplySink) -> Status {
    STATUS_OK
}
unsafe extern "C" fn verify(context: *mut c_void, _: BorrowedBytes) -> Status {
    // SAFETY: context is the live table allocated by the provider factory.
    if unsafe { (*context.cast::<Table>()).accepted } {
        STATUS_OK
    } else {
        STATUS_INVALID_ARGUMENT
    }
}
unsafe extern "C" fn permission(context: *mut c_void, _: BorrowedBytes, _: u32) -> Status {
    // SAFETY: same live table ownership as verify.
    unsafe { verify(context, BorrowedBytes::new(&[])) }
}
unsafe extern "C" fn bind(
    context: *mut c_void,
    _: BorrowedBytes,
    _: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    // SAFETY: the fixture flag and output storage outlive the synchronous bind.
    let accepted = unsafe { *context.cast::<bool>() };
    let mut table = Box::new(Table {
        api: IdentityApiV1_1 {
            v1_0: IdentityApi {
                base: NativeInterface {
                    header: AbiHeader {
                        size: size_of::<IdentityApiV1_1>() as u32,
                        major: 1,
                        minor: 1,
                    },
                    context: std::ptr::null_mut(),
                    release: Some(release),
                },
                certificate: bytes,
                sign,
                verify_client: verify,
            },
            check_permission: permission,
        },
        accepted,
    });
    table.api.v1_0.base.context = (&mut *table as *mut Table).cast();
    // SAFETY: API is the first field and output is writable by the ABI contract.
    unsafe { *output = Box::into_raw(table).cast() };
    STATUS_OK
}

#[test]
fn unchanged_identity_consumer_uses_selected_replacement_provider() {
    for (owner, accepted) in [("default", true), ("replacement", false)] {
        let service = "example.identity/1";
        let configuration = RuntimeConfig {
            principals: [
                (
                    owner.into(),
                    Grants {
                        native_provide: [service.into()].into(),
                        ..Default::default()
                    },
                ),
                (
                    "consumer".into(),
                    Grants {
                        native_use: [service.into()].into(),
                        ..Default::default()
                    },
                ),
            ]
            .into(),
            service_selections: ServiceSelections {
                native: [(
                    service.into(),
                    Some(SelectedService {
                        provider: owner.into(),
                        contract: identity::CONTRACT.into(),
                    }),
                )]
                .into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&configuration).unwrap();
        let mut runtime = std::ptr::null_mut();
        // SAFETY: configuration and output are valid during bootstrap.
        assert_eq!(
            unsafe { create_runtime(BorrowedBytes::new(&bytes), &mut runtime) },
            STATUS_OK
        );
        {
            // SAFETY: endpoints and fixture outlive binding and runtime operations.
            let provider = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, owner)) };
            // SAFETY: this consumer endpoint has the same runtime lifetime.
            let consumer = unsafe { RuntimeClient::from_endpoint(endpoint(runtime, "consumer")) };
            let factory = NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: (&accepted as *const bool).cast_mut().cast(),
                bind: Some(bind),
            };
            // SAFETY: factory promises this exact ABI and remains live through release.
            unsafe { identity::register(&provider, service, &factory) }.unwrap();
            // SAFETY: the provider and runtime remain live until the binding is dropped.
            let binding = unsafe { identity::bind(&consumer, service) }.unwrap();
            // SAFETY: the selected identity contract promises this exact table layout.
            let api = unsafe { binding.table::<IdentityApiV1_1>() }.unwrap();
            let context = api.v1_0.base.context;
            // SAFETY: callback and borrowed bytes are live for this synchronous call.
            let result = unsafe { (api.check_permission)(context, BorrowedBytes::new(b"peer"), 3) };
            assert_eq!(result == STATUS_OK, accepted);
        }
        // SAFETY: all endpoint clients and the binding have been dropped.
        unsafe { destroy_runtime(runtime) };
    }
}
