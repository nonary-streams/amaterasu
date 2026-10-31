//! Signed control-provider fixture. All endpoints are fixed to local loopback.
use amaterasu_media::*;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient, abi};
use serde_json::{Value, json};
use std::ffi::c_void;

const SERVICE: &str = "amaterasu.control-transport/1";

pub struct ControlProbe {
    original: u64,
    pending: u64,
    opening: bool,
    cleaning: bool,
}

impl ControlProbe {
    pub fn start(runtime: &RuntimeClient, original: u64, request: Value) -> ModuleResult<Self> {
        let opening = request["action"] == "control_open";
        let payload = if opening {
            json!({
                "action": "open",
                "peer_ip": "127.0.0.1",
                "connect_data": request["connect_data"],
                "key_hex": request["key_hex"],
                "operation_id": format!("control-fixture-{original}"),
            })
        } else {
            json!({"action": "close", "stream": request["stream"]})
        };
        let pending = request_control(runtime, Some(original), payload)?;
        Ok(Self {
            original,
            pending,
            opening,
            cleaning: false,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        let response = runtime.send_ephemeral(Command::TakeResult {
            request: self.pending,
        })?;
        if matches!(response, Response::Pending) {
            return Ok(false);
        }
        if self.cleaning {
            return Ok(true);
        }
        let payload = match response {
            Response::Result { payload } => payload,
            _ => json!({"ok": false, "code": "fixture_control_failed"}),
        };
        let created = self.opening.then(|| payload["stream"].as_u64()).flatten();
        let complete = runtime.send_ephemeral(Command::Complete {
            request: self.original,
            payload,
        });
        if !matches!(complete, Ok(Response::Ok))
            && let Some(stream) = created
        {
            self.pending =
                request_control(runtime, None, json!({"action": "close", "stream": stream}))?;
            self.cleaning = true;
            return Ok(false);
        }
        Ok(true)
    }
}

fn request_control(
    runtime: &RuntimeClient,
    parent: Option<u64>,
    payload: Value,
) -> ModuleResult<u64> {
    let operation_id = format!(
        "control-fixture-{}-{}",
        payload["action"].as_str().unwrap_or("unknown"),
        parent.or_else(|| payload["stream"].as_u64()).unwrap_or(0)
    );
    match runtime.send_ephemeral(Command::Request {
        service: SERVICE.into(),
        payload,
        parent,
        timeout_ms: 2000,
        operation_id,
    })? {
        Response::Accepted { id } => Ok(id),
        _ => Err("control fixture request rejected".into()),
    }
}

#[derive(Default, PartialEq, Eq)]
struct Capture {
    kind: u32,
    channel: u32,
    delivery: u32,
    sequence: u64,
    bytes: Vec<u8>,
    calls: u32,
    reject: bool,
}

unsafe extern "C" fn capture(context: *mut c_void, message: *const ControlMessage) -> abi::Status {
    if context.is_null() || message.is_null() {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: caller supplies live local capture context; provider borrows its
    // initialized message/payload only until this callback returns.
    let capture = unsafe { &mut *context.cast::<Capture>() };
    let message = unsafe { &*message };
    let Ok(bytes) = (unsafe { message.bytes() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    capture.calls += 1;
    capture.kind = message.message_type;
    capture.channel = message.channel;
    capture.delivery = message.delivery;
    capture.sequence = message.sequence;
    capture.bytes = bytes.to_vec();
    if capture.reject {
        MEDIA_BUSY
    } else {
        abi::STATUS_OK
    }
}

pub fn echo(runtime: &RuntimeClient, stream: u64) -> ModuleResult<Value> {
    // SAFETY: signed dependency retains provider code until this local binding
    // is released; every packet allocation is borrowed synchronously below.
    unsafe {
        let binding = amaterasu_module_sdk::transport::bind_control(runtime, SERVICE)?;
        let api = binding.table::<ControlTransportApi>()?;
        let mut first = Capture {
            reject: true,
            ..Capture::default()
        };
        let receive = (api.receive)(
            api.base.context,
            stream,
            ControlMessageSink {
                context: (&mut first as *mut Capture).cast(),
                write: Some(capture),
            },
        );
        if receive == MEDIA_PENDING {
            return Ok(json!({"ok": true, "pending": true}));
        }
        if receive != MEDIA_BUSY || first.calls != 1 {
            return Err(format!("control receive failed: {receive}"));
        }
        let mut second = Capture::default();
        let receive = (api.receive)(
            api.base.context,
            stream,
            ControlMessageSink {
                context: (&mut second as *mut Capture).cast(),
                write: Some(capture),
            },
        );
        first.reject = false;
        if receive != abi::STATUS_OK || first != second {
            return Err("control sink rejection did not retain the message".into());
        }
        let message = ControlMessage {
            message_type: second.kind,
            channel: second.channel,
            delivery: CONTROL_RELIABLE,
            reserved: 0,
            data: second.bytes.as_ptr(),
            len: second.bytes.len(),
            sequence: 0,
        };
        let sent = (api.send)(api.base.context, stream, &message);
        if sent != abi::STATUS_OK {
            return Err(format!("control echo send failed: {sent}"));
        }
        Ok(json!({
            "ok": true, "pending": false, "message_type": second.kind,
            "channel": second.channel, "sequence": second.sequence,
            "bytes": second.bytes.len(), "retry_preserved": true,
        }))
    }
}
