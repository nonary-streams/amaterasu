use super::CONTROL_CONTRACT;
use crate::{
    ModuleResult, NativeBinding, RuntimeClient,
    abi::{BorrowedBytes, ReplySink, STATUS_INVALID_ARGUMENT, STATUS_OK, Status},
};
use amaterasu_contracts::control::{
    ClientControl, HostControl, HostFeedbackDelivery, HostFeedbackMessage,
};
use amaterasu_media::{
    CONTROL_RELIABLE, CONTROL_UNRELIABLE, CONTROL_UNSEQUENCED, ControlMessage, ControlMessageSink,
    ControlTransportApiV1_1, MAX_SEMANTIC_CONTROL_BYTES, MEDIA_UNSUPPORTED,
};
use std::ffi::c_void;

pub use amaterasu_contracts::control;

pub struct ControlCodec(NativeBinding);

/// # Safety
/// The runtime and provider must outlive the binding and its calls.
pub unsafe fn bind_control_codec(
    runtime: &RuntimeClient,
    service: &str,
) -> ModuleResult<ControlCodec> {
    unsafe {
        runtime.bind_native_typed(
            service,
            CONTROL_CONTRACT,
            1,
            1,
            size_of::<ControlTransportApiV1_1>() as u32,
        )
    }
    .map(ControlCodec)
}

/// Accumulates at most one result and rejects any provider callback duplication.
struct Reply<T> {
    value: Option<T>,
    invalid: bool,
}

impl<T> Default for Reply<T> {
    fn default() -> Self {
        Self {
            value: None,
            invalid: false,
        }
    }
}

impl ControlCodec {
    pub fn binding(&self) -> &NativeBinding {
        &self.0
    }

    pub fn decode_client(
        &self,
        message_type: u16,
        channel: u8,
        delivery: u32,
        payload: &[u8],
        sequence: u64,
    ) -> ModuleResult<ClientControl<'static>> {
        let message = ControlMessage {
            message_type: message_type.into(),
            channel: channel.into(),
            delivery,
            reserved: 0,
            data: payload.as_ptr(),
            len: payload.len(),
            sequence,
        };
        message.validate().map_err(|_| "invalid control message")?;
        let mut reply = Reply::<ClientControl<'static>>::default();
        // SAFETY: the typed binding establishes the native layout. All inputs
        // and the synchronous reply context remain live through this call.
        let api = unsafe { self.0.table::<ControlTransportApiV1_1>() }?;
        let status = unsafe {
            (api.decode_client)(
                self.0.context(),
                &message,
                ReplySink {
                    context: (&mut reply as *mut Reply<ClientControl<'static>>).cast(),
                    write: client_reply,
                },
            )
        };
        if status != STATUS_OK || reply.invalid {
            return Err("control provider failed to interpret a client record".into());
        }
        reply
            .value
            .ok_or("control provider omitted a client record".into())
    }

    /// An explicit unsupported response means the provider produced no record.
    pub fn encode_host(&self, event: &HostControl) -> ModuleResult<Option<HostFeedbackMessage>> {
        let json = serde_json::to_vec(event).map_err(|error| error.to_string())?;
        if json.len() > MAX_SEMANTIC_CONTROL_BYTES {
            return Err("host control event exceeds its limit".into());
        }
        let mut reply = Reply::<HostFeedbackMessage>::default();
        // SAFETY: the typed binding establishes the native layout. The JSON
        // buffer and synchronous callback context both outlive this call.
        let api = unsafe { self.0.table::<ControlTransportApiV1_1>() }?;
        let status = unsafe {
            (api.encode_host)(
                self.0.context(),
                BorrowedBytes::new(&json),
                ControlMessageSink {
                    context: (&mut reply as *mut Reply<HostFeedbackMessage>).cast(),
                    write: Some(host_reply),
                },
            )
        };
        if status == MEDIA_UNSUPPORTED && !reply.invalid && reply.value.is_none() {
            return Ok(None);
        }
        if status != STATUS_OK || reply.invalid {
            return Err("control provider failed to frame a host event".into());
        }
        reply
            .value
            .map(Some)
            .ok_or("control provider omitted a host record".into())
    }
}

unsafe extern "C" fn client_reply(context: *mut c_void, bytes: BorrowedBytes) {
    // SAFETY: this context is supplied by decode_client and lives for the call.
    let reply = unsafe { &mut *context.cast::<Reply<ClientControl<'static>>>() };
    if reply.invalid || reply.value.is_some() || bytes.len > MAX_SEMANTIC_CONTROL_BYTES {
        reply.invalid = true;
        return;
    }
    reply.value = unsafe { bytes.read() }
        .ok()
        .and_then(|bytes| serde_json::from_slice(bytes).ok());
    reply.invalid = reply.value.is_none();
}

unsafe extern "C" fn host_reply(context: *mut c_void, message: *const ControlMessage) -> Status {
    // SAFETY: this context is supplied from encode_host and lives for the call.
    let reply = unsafe { &mut *context.cast::<Reply<HostFeedbackMessage>>() };
    if reply.invalid || reply.value.is_some() || message.is_null() {
        reply.invalid = true;
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the provider lends the record and its bytes to this callback.
    let message = unsafe { &*message };
    let bytes = match unsafe { message.bytes() } {
        Ok(bytes) if bytes.len() <= MAX_SEMANTIC_CONTROL_BYTES => bytes,
        _ => {
            reply.invalid = true;
            return STATUS_INVALID_ARGUMENT;
        }
    };
    let delivery = match message.delivery {
        CONTROL_RELIABLE => HostFeedbackDelivery::Reliable,
        CONTROL_UNRELIABLE => HostFeedbackDelivery::SequencedUnreliable,
        CONTROL_UNSEQUENCED => HostFeedbackDelivery::Unsequenced,
        _ => {
            reply.invalid = true;
            return STATUS_INVALID_ARGUMENT;
        }
    };
    reply.value = Some(HostFeedbackMessage {
        message_type: message.message_type as u16,
        channel: message.channel as u8,
        delivery,
        payload: bytes.to_vec(),
    });
    STATUS_OK
}
