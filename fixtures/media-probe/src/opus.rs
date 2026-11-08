//! Controlled integration exercise of the real signed audio encoder library.
use amaterasu_media::{
    AUDIO_FORMAT_OPUS, AUDIO_PACKET_END_OF_STREAM, AudioEncoderApi, AudioFrame, AudioUnit,
    AudioUnitSink, MEDIA_BUSY, MEDIA_CLOSED, MEDIA_PENDING, PCM_I16,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient, abi};
use serde_json::{Value, json};
use std::ffi::c_void;

const SERVICE: &str = "amaterasu.audio-encode/1";

pub struct OpusProbe {
    original_request: u64,
    pending_request: u64,
    result: Option<Value>,
}

impl OpusProbe {
    pub fn start(runtime: &RuntimeClient, request: u64) -> ModuleResult<Self> {
        let pending_request = submit_control(
            runtime,
            json!({"action":"open","channels":2,"packet_ms":10,"bitrate":128000,
                "operation_id":format!("media-probe-{request}")}),
            format!("media-probe-open-{request}"),
            Some(request),
        )?;
        Ok(Self {
            original_request: request,
            pending_request,
            result: None,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        let payload = match runtime.send_ephemeral(Command::TakeResult {
            request: self.pending_request,
        })? {
            Response::Pending => return Ok(false),
            Response::Result { payload } if payload["ok"] == true => payload,
            response => return Err(format!("Opus fixture control failed: {response:?}")),
        };
        if let Some(result) = self.result.take() {
            runtime.require_success(Command::Complete {
                request: self.original_request,
                payload: result,
            })?;
            return Ok(true);
        }
        let stream = payload["stream"]
            .as_u64()
            .ok_or("Opus open omitted stream")?;
        // Keep test failures visible, but close the allocated stream before replying.
        self.result = Some(match exercise_encoder(runtime, stream) {
            Ok(result) => result,
            Err(error) => json!({"ok":false,"error":error}),
        });
        self.pending_request = submit_control(
            runtime,
            json!({"action":"close","stream":stream}),
            format!("media-probe-close-{}", self.original_request),
            None,
        )?;
        Ok(false)
    }
}

fn submit_control(
    runtime: &RuntimeClient,
    payload: Value,
    operation_id: String,
    parent: Option<u64>,
) -> ModuleResult<u64> {
    match runtime.send_ephemeral(Command::Request {
        service: SERVICE.into(),
        payload,
        timeout_ms: 3000,
        operation_id,
        parent,
    })? {
        Response::Accepted { id } => Ok(id),
        response => Err(format!("Opus fixture request rejected: {response:?}")),
    }
}

fn exercise_encoder(runtime: &RuntimeClient, stream: u64) -> ModuleResult<Value> {
    // SAFETY: the signed dependency supplies this exact media-v1 table. The
    // runtime/provider outlive the binding; all PCM and callbacks are synchronous.
    let binding = unsafe { amaterasu_module_sdk::audio::bind_encoder(runtime, SERVICE) }?;
    let api = unsafe { binding.table::<AudioEncoderApi>() }?;
    let first = verify_partial_input_and_retry(api, stream)?;
    let tail = verify_drain(api, stream)?;
    verify_ended(api, stream)?;
    Ok(json!({
        "ok": true,
        "packets": 2,
        "decoded_frames": first.frames + tail.frames,
        "encoded_bytes": first.bytes.len() + tail.bytes.len(),
        "retry_preserved": true,
        "eos_once": true,
    }))
}

fn verify_partial_input_and_retry(api: &AudioEncoderApi, stream: u64) -> ModuleResult<Packet> {
    let mut received = Delivery::default();

    submit_pcm(api, stream, 240, 0, 1)?;
    require_status(
        receive(api, stream, &mut received),
        MEDIA_PENDING,
        "partial input",
    )?;
    if received.packet.is_some() {
        return Err("partial PCM unexpectedly produced a packet".into());
    }
    submit_pcm(api, stream, 240, 5000, 2)?;

    received.reject = true;
    require_status(
        receive(api, stream, &mut received),
        MEDIA_BUSY,
        "rejected packet",
    )?;
    let first = received
        .packet
        .take()
        .ok_or("encoder did not produce a complete Opus packet")?;
    validate_packet(&first, 0)?;
    received.reject = false;
    require_status(
        receive(api, stream, &mut received),
        abi::STATUS_OK,
        "packet retry",
    )?;
    if received.packet.take().as_ref() != Some(&first) {
        return Err("rejected Opus packet changed before retry".into());
    }
    Ok(first)
}

fn verify_drain(api: &AudioEncoderApi, stream: u64) -> ModuleResult<Packet> {
    let mut received = Delivery::default();
    submit_pcm(api, stream, 120, 10_000, 3)?;
    // SAFETY: the bound table and owned stream remain live for each native call.
    require_status(
        unsafe { (api.drain)(api.base.context, stream) },
        abi::STATUS_OK,
        "drain",
    )?;
    require_status(
        receive(api, stream, &mut received),
        abi::STATUS_OK,
        "padded tail",
    )?;
    let tail = received
        .packet
        .take()
        .ok_or("drain omitted the final padded packet")?;
    validate_packet(&tail, 10_000)?;
    require_status(receive(api, stream, &mut received), abi::STATUS_OK, "EOS")?;
    let eos = received.packet.take().ok_or("drain omitted EOS")?;
    if eos.flags != AUDIO_PACKET_END_OF_STREAM || eos.frames != 0 || !eos.bytes.is_empty() {
        return Err("invalid Opus EOS packet".into());
    }
    Ok(tail)
}

fn verify_ended(api: &AudioEncoderApi, stream: u64) -> ModuleResult<()> {
    let mut received = Delivery::default();
    require_status(
        receive(api, stream, &mut received),
        MEDIA_CLOSED,
        "after EOS",
    )?;
    require_status(
        // SAFETY: the table and this caller-owned stream remain live during drain.
        unsafe { (api.drain)(api.base.context, stream) },
        abi::STATUS_OK,
        "repeated drain",
    )?;
    require_status(
        receive(api, stream, &mut received),
        MEDIA_CLOSED,
        "duplicate EOS",
    )?;
    let bytes = pcm(120);
    let frame = input_frame(&bytes, 120, 12_500, 4);
    require_status(
        // SAFETY: the initialized PCM and table outlive the synchronous submit.
        unsafe { (api.submit)(api.base.context, stream, &frame) },
        MEDIA_CLOSED,
        "input after drain",
    )?;
    Ok(())
}

fn validate_packet(packet: &Packet, timestamp_us: u64) -> ModuleResult<()> {
    if packet.flags & AUDIO_PACKET_END_OF_STREAM != 0
        || packet.frames != 480
        || packet.timestamp_us != timestamp_us
        || packet.bytes.is_empty()
    {
        return Err("Opus packet duration, timestamp or payload differs".into());
    }
    Ok(())
}

fn submit_pcm(
    api: &AudioEncoderApi,
    stream: u64,
    frames: u32,
    timestamp_us: u64,
    sequence: u64,
) -> ModuleResult<()> {
    let bytes = pcm(frames);
    let frame = input_frame(&bytes, frames, timestamp_us, sequence);
    // SAFETY: the initialized PCM vector and table remain alive for this call.
    require_status(
        unsafe { (api.submit)(api.base.context, stream, &frame) },
        abi::STATUS_OK,
        "PCM submit",
    )
}

fn pcm(frames: u32) -> Vec<u8> {
    (0..frames)
        .flat_map(|index| {
            let sample = if index % 128 < 64 { 2000i16 } else { -2000i16 };
            let [low, high] = sample.to_le_bytes();
            [low, high, low, high]
        })
        .collect()
}

fn input_frame(bytes: &[u8], frames: u32, timestamp_us: u64, sequence: u64) -> AudioFrame {
    AudioFrame {
        sample_rate: 48_000,
        channels: 2,
        format: amaterasu_media::AudioFormatId::standard(u64::from(PCM_I16)),
        sample_bytes: 2,
        packing: amaterasu_media::AUDIO_PACKING_INTERLEAVED,
        frames,
        flags: 0,
        data: bytes.as_ptr(),
        len: bytes.len(),
        timestamp_us,
        sequence,
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Packet {
    pub(super) flags: u32,
    pub(super) frames: u32,
    pub(super) timestamp_us: u64,
    pub(super) sequence: u64,
    pub(super) bytes: Vec<u8>,
}

#[derive(Default)]
struct Delivery {
    reject: bool,
    packet: Option<Packet>,
}

fn receive(api: &AudioEncoderApi, stream: u64, delivery: &mut Delivery) -> abi::Status {
    let sink = AudioUnitSink {
        context: (delivery as *mut Delivery).cast(),
        write: collect_packet,
    };
    // SAFETY: exclusive callback state outlives this synchronous provider call.
    unsafe { (api.receive)(api.base.context, stream, sink) }
}

unsafe extern "C" fn collect_packet(context: *mut c_void, packet: *const AudioUnit) -> abi::Status {
    // SAFETY: the provider supplies live packet metadata/bytes; the fixture owns
    // the callback context exclusively and copies all output before returning.
    let delivery = unsafe { &mut *context.cast::<Delivery>() };
    let packet = unsafe { &*packet };
    if packet.validate().is_err() || packet.format != AUDIO_FORMAT_OPUS || packet.metadata.len != 0
    {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    let bytes = match unsafe { packet.payload.read() } {
        Ok(bytes) => bytes,
        _ => return abi::STATUS_INVALID_ARGUMENT,
    };
    let samples = u128::from(packet.duration_us) * 48_000;
    if samples % 1_000_000 != 0 {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    let Ok(frames) = u32::try_from(samples / 1_000_000) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    delivery.packet = Some(Packet {
        flags: packet.flags,
        frames,
        timestamp_us: packet.timestamp_us,
        sequence: packet.sequence,
        bytes: bytes.to_vec(),
    });
    if delivery.reject {
        MEDIA_BUSY
    } else {
        abi::STATUS_OK
    }
}

fn require_status(actual: abi::Status, expected: abi::Status, operation: &str) -> ModuleResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "Opus {operation}: expected {expected}, received {actual}"
        ))
    }
}

/// Produce four real CBR packets for the native UDP transport integration.
pub(super) fn encode_transport_packets(
    runtime: &RuntimeClient,
    stream: u64,
) -> ModuleResult<Vec<Packet>> {
    // SAFETY: the signed dependency supplies this exact table and outlives its
    // binding; all PCM input and packet callbacks remain synchronous.
    let binding = unsafe { amaterasu_module_sdk::audio::bind_encoder(runtime, SERVICE) }?;
    let api = unsafe { binding.table::<AudioEncoderApi>() }?;
    let mut packets = Vec::with_capacity(4);
    for index in 0..4 {
        submit_pcm(api, stream, 480, index * 10_000, index + 1)?;
        let mut delivery = Delivery::default();
        require_status(
            receive(api, stream, &mut delivery),
            abi::STATUS_OK,
            "transport encode",
        )?;
        let packet = delivery
            .packet
            .ok_or("Opus encoder omitted transport packet")?;
        validate_packet(&packet, index * 10_000)?;
        if packet.bytes.len() != 160 {
            return Err("CBR Opus packet was not 160 bytes".into());
        }
        packets.push(packet);
    }
    Ok(packets)
}
