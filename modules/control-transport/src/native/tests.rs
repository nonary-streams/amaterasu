use super::*;
use crate::controller::tests::{configured, request};
use crate::model::Operation;

fn query() -> NativeInterfaceQuery {
    NativeInterfaceQuery {
        header: AbiHeader::of::<NativeInterfaceQuery>(),
        interface_major: INTERFACE_MAJOR,
        min_minor: INTERFACE_MINOR,
        min_size: size_of::<ControlTransportApi>() as u32,
    }
}

unsafe fn binding(factory: &Factory, owner: &str) -> *const ControlTransportApi {
    let mut output = std::ptr::null();
    // SAFETY: all arguments remain live throughout bind; caller releases the result.
    assert_eq!(
        unsafe {
            bind(
                factory.api.context,
                BorrowedBytes::new(owner.as_bytes()),
                &query(),
                &mut output,
            )
        },
        STATUS_OK
    );
    output.cast()
}

#[test]
fn invalid_bind_clears_output_and_rejects_versions() {
    let factory = Factory::new(Arc::new(Mutex::new(configured())));
    let mut output = std::ptr::dangling::<NativeInterface>();
    // SAFETY: writable output is valid; null context must be rejected before access.
    assert_eq!(
        unsafe {
            bind(
                std::ptr::null_mut(),
                BorrowedBytes::new(b"alice"),
                &query(),
                &mut output,
            )
        },
        STATUS_INVALID_ARGUMENT
    );
    assert!(output.is_null());
    let mut unsupported = query();
    unsupported.interface_major += 1;
    assert_eq!(
        unsafe {
            bind(
                factory.api.context,
                BorrowedBytes::new(b"alice"),
                &unsupported,
                &mut output,
            )
        },
        MEDIA_UNSUPPORTED
    );
    assert!(output.is_null());
}

#[test]
fn native_handles_copy_owner_observe_shutdown_and_release_exactly_once() {
    let controller = Arc::new(Mutex::new(configured()));
    let id = controller
        .lock()
        .unwrap()
        .handle("alice", Operation::Open(request()))
        .unwrap()
        .created
        .unwrap();
    let factory = Factory::new(controller.clone());
    let owner = String::from("alice");
    // SAFETY: factory lives through bind; each resulting handle is released below.
    let alice = unsafe { binding(&factory, &owner) };
    let bob = unsafe { binding(&factory, "bob") };
    drop(owner);
    drop(factory);
    assert_eq!(Arc::strong_count(&controller), 3);
    let sink = ControlMessageSink {
        context: std::ptr::null_mut(),
        write: None,
    };
    // SAFETY: both bindings remain live and sink has no callback allocation.
    unsafe {
        assert_eq!(
            ((*bob).receive)((*bob).base.context, id, sink),
            MEDIA_CLOSED
        );
        assert_eq!(
            ((*alice).receive)((*alice).base.context, id, sink),
            STATUS_INVALID_ARGUMENT
        );
        let lock = controller.lock().unwrap();
        assert_eq!(
            ((*alice).receive)((*alice).base.context, id, sink),
            MEDIA_BUSY
        );
        drop(lock);
        controller.lock().unwrap().shutdown();
        assert_eq!(
            ((*alice).receive)((*alice).base.context, id, sink),
            MEDIA_CLOSED
        );
        ((*alice).base.release.unwrap())((*alice).base.context);
        ((*bob).base.release.unwrap())((*bob).base.context);
    }
    assert_eq!(Arc::strong_count(&controller), 1);
}

#[test]
fn semantic_control_tail_decodes_idr_and_owns_hdr_wire_bytes() {
    let mut event = None;
    unsafe extern "C" fn receive_event(context: *mut c_void, json: BorrowedBytes) {
        let output = unsafe { &mut *context.cast::<Option<serde_json::Value>>() };
        *output = Some(serde_json::from_slice(unsafe { json.read().unwrap() }).unwrap());
    }
    let body = [0, 0];
    let request = ControlMessage {
        message_type: 0x0302,
        channel: 0,
        delivery: CONTROL_RELIABLE,
        reserved: 0,
        data: body.as_ptr(),
        len: 2,
        sequence: 42,
    };
    let status = unsafe {
        decode_client(
            std::ptr::null_mut(),
            &request,
            ReplySink {
                context: (&mut event as *mut Option<serde_json::Value>).cast(),
                write: receive_event,
            },
        )
    };
    assert_eq!(status, STATUS_OK);
    assert_eq!(event, Some(serde_json::json!({"type":"request_idr"})));

    struct Framed {
        kind: u32,
        channel: u32,
        payload: Vec<u8>,
    }

    unsafe extern "C" fn receive_frame(
        context: *mut c_void,
        message: *const ControlMessage,
    ) -> Status {
        let output = unsafe { &mut *context.cast::<Option<Framed>>() };
        let message = unsafe { &*message };
        *output = Some(Framed {
            kind: message.message_type,
            channel: message.channel,
            payload: unsafe { message.bytes().unwrap() }.to_vec(),
        });
        STATUS_OK
    }
    let mut fields = [0; 12];
    fields[0] = 0x1234;
    let hdr = amaterasu_module_sdk::transport::control::HostControl::HdrMetadata { fields };
    let json = serde_json::to_vec(&hdr).unwrap();
    let mut framed: Option<Framed> = None;
    let status = unsafe {
        encode_host(
            std::ptr::null_mut(),
            BorrowedBytes::new(&json),
            ControlMessageSink {
                context: (&mut framed as *mut Option<Framed>).cast(),
                write: Some(receive_frame),
            },
        )
    };
    assert_eq!(status, STATUS_OK);
    let framed = framed.unwrap();
    assert_eq!((framed.kind, framed.channel), (0x010e, 0));
    assert_eq!(&framed.payload[..3], &[1, 0x34, 0x12]);
    assert_eq!(framed.payload.len(), 27);
    assert!(framed.payload[3..].iter().all(|byte| *byte == 0));
}
