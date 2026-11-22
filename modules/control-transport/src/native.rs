//! Native handles retain only the synchronized controller and a copied principal.
use crate::controller::Controller;
use amaterasu_media::*;
use amaterasu_module_sdk::abi::*;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, TryLockError},
};

pub type Shared = Arc<Mutex<Controller>>;

pub struct Factory {
    pub api: NativeServiceFactory,
    controller: Shared,
}

impl Factory {
    pub fn new(controller: Shared) -> Box<Self> {
        let mut factory = Box::new(Self {
            api: NativeServiceFactory {
                header: AbiHeader::of::<NativeServiceFactory>(),
                context: std::ptr::null_mut(),
                bind: Some(bind),
            },
            controller,
        });
        factory.api.context = (&mut *factory as *mut Self).cast();
        factory
    }
}

struct Binding {
    api: ControlTransportApiV1_1,
    controller: Shared,
    owner: String,
}

unsafe extern "C" fn bind(
    context: *mut c_void,
    principal: BorrowedBytes,
    query: *const NativeInterfaceQuery,
    output: *mut *const NativeInterface,
) -> Status {
    guard(|| {
        if output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: a non-null output is writable under the factory ABI contract.
        unsafe {
            *output = std::ptr::null();
        }
        if context.is_null() || query.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: runtime supplies a live factory and initialized query/output.
        // The principal is borrowed only for this callback and copied below.
        unsafe {
            let query = &*query;
            if !query.header.supports::<NativeInterfaceQuery>()
                || query.interface_major != INTERFACE_MAJOR
                || query.min_minor > 1
                || query.min_size as usize > size_of::<ControlTransportApiV1_1>()
            {
                return MEDIA_UNSUPPORTED;
            }
            let Some(owner) = principal
                .read()
                .ok()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .filter(|owner| crate::model::valid_name(owner))
            else {
                return STATUS_INVALID_ARGUMENT;
            };
            let mut binding = Box::new(Binding {
                api: ControlTransportApiV1_1 {
                    v1_0: ControlTransportApi {
                        base: NativeInterface {
                            header: AbiHeader {
                                size: size_of::<ControlTransportApiV1_1>() as u32,
                                major: INTERFACE_MAJOR,
                                minor: 1,
                            },
                            context: std::ptr::null_mut(),
                            release: Some(release),
                        },
                        send,
                        receive,
                    },
                    decode_client,
                    encode_host,
                },
                controller: (&*context.cast::<Factory>()).controller.clone(),
                owner: owner.into(),
            });
            binding.api.v1_0.base.context = (&mut *binding as *mut Binding).cast();
            *output = &(*Box::into_raw(binding)).api.v1_0.base;
        }
        STATUS_OK
    })
}

unsafe extern "C" fn release(context: *mut c_void) {
    let _ = guard(|| {
        if !context.is_null() {
            // SAFETY: runtime releases this unique binding once after all calls end.
            drop(unsafe { Box::from_raw(context.cast::<Binding>()) });
        }
        STATUS_OK
    });
}

unsafe extern "C" fn send(
    context: *mut c_void,
    stream: u64,
    message: *const ControlMessage,
) -> Status {
    guard(|| {
        if message.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: consumer keeps context, message and initialized payload alive.
        unsafe {
            with_controller(context, |controller, owner| {
                controller.send(owner, stream, &*message)
            })
        }
    })
}

unsafe extern "C" fn receive(
    context: *mut c_void,
    stream: u64,
    sink: ControlMessageSink,
) -> Status {
    guard(|| {
        // SAFETY: consumer keeps its binding and callback context live through return.
        unsafe {
            with_controller(context, |controller, owner| {
                controller.receive(owner, stream, sink)
            })
        }
    })
}

// Both conversions are synchronous, bounded, and effect free. They use no
// stream controller lock and do not admit or consume a transport message.
unsafe extern "C" fn decode_client(
    _: *mut c_void,
    message: *const ControlMessage,
    sink: ReplySink,
) -> Status {
    use amaterasu_gamestream::{
        control::{self, Message, Profile},
        crypto::Direction,
    };
    use amaterasu_module_sdk::transport::control::{ClientControl, Input};
    guard(|| {
        // SAFETY: the caller lends the authenticated message and its payload.
        let Some(message) = (unsafe { message.as_ref() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(payload) = (unsafe { message.bytes() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let result = match control::decode(
            Profile::SunshineModern,
            Direction::ClientToHost,
            message.message_type as u16,
            payload,
        ) {
            Ok(Message::Ping { .. } | Message::Start) => ClientControl::Keepalive,
            Ok(Message::RequestIdr | Message::InvalidateReferences { .. }) => {
                ClientControl::RequestIdr
            }
            Ok(Message::Input(Input::Unsupported { magic, .. })) => {
                ClientControl::Input(Input::Unsupported {
                    magic,
                    payload: std::borrow::Cow::Borrowed(&[]),
                })
            }
            Ok(Message::Input(input)) => ClientControl::Input(input),
            Ok(_) => ClientControl::Unsupported,
            Err(_) => ClientControl::Invalid,
        };
        // The default protocol yields small fixed-size input records and at
        // most 32 text bytes. Keep serialization off the heap on each input.
        let mut bytes = [0; 512];
        let mut output = std::io::Cursor::new(bytes.as_mut_slice());
        if serde_json::to_writer(&mut output, &result).is_err() {
            return STATUS_INVALID_ARGUMENT;
        }
        let length = output.position() as usize;
        // SAFETY: the semantic result is borrowed only during the callback.
        unsafe { sink.send(&bytes[..length]) };
        STATUS_OK
    })
}

unsafe extern "C" fn encode_host(
    _: *mut c_void,
    event: BorrowedBytes,
    sink: ControlMessageSink,
) -> Status {
    use amaterasu_gamestream::control::encode_host_feedback;
    use amaterasu_module_sdk::transport::control::{
        HostControl, HostFeedbackDelivery, HostFeedbackMessage,
    };
    guard(|| {
        let Some(write) = sink.write else {
            return STATUS_INVALID_ARGUMENT;
        };
        if event.len > MAX_SEMANTIC_CONTROL_BYTES {
            return STATUS_INVALID_ARGUMENT;
        }
        // SAFETY: the caller lends the JSON bytes through this call.
        let Ok(event) = (unsafe { event.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(event) = serde_json::from_slice(event) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let message = match event {
            HostControl::Feedback(feedback) => match encode_host_feedback(feedback) {
                Ok(message) => message,
                Err(_) => return STATUS_INVALID_ARGUMENT,
            },
            HostControl::HdrMetadata { fields } => {
                let mut payload = vec![1];
                for field in fields {
                    payload.extend_from_slice(&field.to_le_bytes());
                }
                payload.extend_from_slice(&0u16.to_le_bytes());
                HostFeedbackMessage {
                    message_type: 0x010e,
                    channel: 0,
                    delivery: HostFeedbackDelivery::Reliable,
                    payload,
                }
            }
        };
        let raw = ControlMessage {
            message_type: message.message_type.into(),
            channel: message.channel.into(),
            delivery: match message.delivery {
                HostFeedbackDelivery::Reliable => CONTROL_RELIABLE,
                HostFeedbackDelivery::SequencedUnreliable => CONTROL_UNRELIABLE,
                HostFeedbackDelivery::Unsequenced => CONTROL_UNSEQUENCED,
            },
            reserved: 0,
            data: message.payload.as_ptr(),
            len: message.payload.len(),
            sequence: 0,
        };
        // SAFETY: the fully framed record is borrowed only during the callback.
        unsafe { write(sink.context, &raw) }
    })
}

unsafe fn with_controller(
    context: *mut c_void,
    operation: impl FnOnce(&mut Controller, &str) -> Status,
) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: caller holds a live binding; the immutable handle may be shared.
    let binding = unsafe { &*context.cast::<Binding>() };
    match binding.controller.try_lock() {
        Ok(mut controller) => operation(&mut controller, &binding.owner),
        Err(TryLockError::WouldBlock) => MEDIA_BUSY,
        Err(TryLockError::Poisoned(_)) => MEDIA_BACKEND_ERROR,
    }
}

fn guard(operation: impl FnOnce() -> Status) -> Status {
    let _priority = amaterasu_module_sdk::threading::ScopedThreadPriority::best_effort(
        amaterasu_module_sdk::threading::ThreadPriority::Critical,
    );
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(MEDIA_BACKEND_ERROR)
}

#[cfg(test)]
mod tests;
