//! Native calls borrow copied headers only synchronously. Bindings are retained
//! until every pending call and callback has returned, then released by the SDK.
use super::*;
use amaterasu_module_sdk::{NativeBinding, abi::*};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

pub(super) struct NativeBackend {
    video: NativeBinding,
    capture: Option<NativeBinding>,
    encoder: Option<NativeBinding>,
    media: NativeBinding,
    control: NativeBinding,
    handles: Handles,
}

impl NativeBackend {
    pub(super) fn new(runtime: &RuntimeClient, handles: Handles) -> PlaneResult<Self> {
        let video = bind::<VideoApi>(runtime, "amaterasu.video/1")?;
        let (capture, encoder) = match (
            handles.stream_audio,
            handles.audio_capture,
            handles.audio_encoder,
        ) {
            (true, Some(_), Some(_)) => (
                Some(bind::<AudioCaptureApi>(runtime, "amaterasu.audio/1")?),
                Some(bind::<AudioEncoderApi>(
                    runtime,
                    "amaterasu.audio-encode/1",
                )?),
            ),
            (false, None, None) => (None, None),
            _ => return Err("invalid_audio_handles"),
        };
        Ok(Self {
            video,
            capture,
            encoder,
            media: bind::<MediaTransportApi>(runtime, "amaterasu.media-transport/1")?,
            control: bind::<ControlTransportApi>(runtime, "amaterasu.control-transport/1")?,
            handles,
        })
    }
}

fn bind<T>(runtime: &RuntimeClient, service: &str) -> PlaneResult<NativeBinding> {
    // SAFETY: signed dependencies retain these exact C interfaces until plane drop.
    let binding = unsafe { runtime.bind_native(service, 1, 0, size_of::<T>() as u32) }
        .map_err(|_| "native_binding_failed")?;
    // SAFETY: each private call site supplies the documented service's exact table.
    unsafe { binding.table::<T>() }.map_err(|_| "native_layout_mismatch")?;
    Ok(binding)
}

impl Backend for NativeBackend {
    fn control(&mut self) -> PlaneResult<Option<ControlRecord>> {
        let mut output = Delivery::default();
        // SAFETY: the owned binding and stack sink remain live for one callback.
        let api = unsafe { self.control.table::<ControlTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        let status = unsafe {
            (api.receive)(
                self.control.context(),
                self.handles.control_transport,
                ControlMessageSink {
                    context: (&mut output as *mut Delivery<ControlRecord>).cast(),
                    write: Some(copy_control),
                },
            )
        };
        output.finish(status)
    }

    fn video(&mut self) -> PlaneResult<Option<VideoPacket>> {
        let mut output = Delivery::default();
        // SAFETY: binding and output are retained throughout synchronous delivery.
        let api =
            unsafe { self.video.table::<VideoApi>() }.map_err(|_| "native_layout_mismatch")?;
        let status = unsafe {
            (api.receive)(
                self.video.context(),
                self.handles.video,
                PacketSink {
                    context: (&mut output as *mut Delivery<VideoPacket>).cast(),
                    write: copy_video,
                },
            )
        };
        output.finish(status)
    }

    fn pcm(&mut self) -> PlaneResult<Option<PcmPacket>> {
        let mut output: Delivery<PcmPacket> = Delivery::default();
        // SAFETY: callback copies initialized PCM before the provider borrow ends.
        let capture = self.capture.as_ref().ok_or("audio_disabled")?;
        let stream = self.handles.audio_capture.ok_or("audio_disabled")?;
        let api =
            unsafe { capture.table::<AudioCaptureApi>() }.map_err(|_| "native_layout_mismatch")?;
        let status = unsafe {
            (api.read_audio)(
                capture.context(),
                stream,
                AudioSink {
                    context: (&mut output as *mut Delivery<PcmPacket>).cast(),
                    write: copy_pcm,
                },
            )
        };
        let packet = output.finish(status)?;
        if packet
            .as_ref()
            .is_some_and(|packet| packet.header().channels != self.handles.audio_channels)
        {
            return Err("unsupported_audio_channels");
        }
        Ok(packet)
    }

    fn audio(&mut self) -> PlaneResult<Option<AudioPacket>> {
        let mut output: Delivery<AudioPacket> = Delivery::default();
        // SAFETY: stack output and binding outlive the synchronous encoder call.
        let encoder = self.encoder.as_ref().ok_or("audio_disabled")?;
        let stream = self.handles.audio_encoder.ok_or("audio_disabled")?;
        let api =
            unsafe { encoder.table::<AudioEncoderApi>() }.map_err(|_| "native_layout_mismatch")?;
        let status = unsafe {
            (api.receive)(
                encoder.context(),
                stream,
                AudioPacketSink {
                    context: (&mut output as *mut Delivery<AudioPacket>).cast(),
                    write: copy_audio,
                },
            )
        };
        if status == MEDIA_BACKEND_ERROR {
            Err("audio_encoder_failed")
        } else {
            let packet = output.finish(status)?;
            if packet
                .as_ref()
                .is_some_and(|packet| packet.header().channels != self.handles.audio_channels)
            {
                return Err("unsupported_audio_channels");
            }
            Ok(packet)
        }
    }

    fn submit_video(&mut self, packet: &EncodedPacket) -> PlaneResult<bool> {
        // SAFETY: packet points into an owned assembly retained until return.
        let api = unsafe { self.media.table::<MediaTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        accepted(unsafe {
            (api.submit_video)(self.media.context(), self.handles.media_transport, packet)
        })
    }

    fn submit_pcm(&mut self, frame: &AudioFrame) -> PlaneResult<bool> {
        // SAFETY: frame points into the plane's retained initialized PCM allocation.
        let encoder = self.encoder.as_ref().ok_or("audio_disabled")?;
        let stream = self.handles.audio_encoder.ok_or("audio_disabled")?;
        let api =
            unsafe { encoder.table::<AudioEncoderApi>() }.map_err(|_| "native_layout_mismatch")?;
        encoder_accepted(unsafe { (api.submit)(encoder.context(), stream, frame) })
    }

    fn submit_audio(&mut self, packet: &EncodedAudioPacket) -> PlaneResult<bool> {
        // SAFETY: packet allocation remains owned by the plane until return.
        let api = unsafe { self.media.table::<MediaTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        accepted(unsafe {
            (api.submit_audio)(self.media.context(), self.handles.media_transport, packet)
        })
    }

    fn drain_encoder(&mut self) -> PlaneResult<bool> {
        // SAFETY: binding and owned stream ID are retained by the pipeline.
        let encoder = self.encoder.as_ref().ok_or("audio_disabled")?;
        let stream = self.handles.audio_encoder.ok_or("audio_disabled")?;
        let api =
            unsafe { encoder.table::<AudioEncoderApi>() }.map_err(|_| "native_layout_mismatch")?;
        encoder_accepted(unsafe { (api.drain)(encoder.context(), stream) })
    }

    fn drain_transport(&mut self) -> PlaneResult<bool> {
        // SAFETY: no callback or borrowed allocation escapes this call.
        let api = unsafe { self.media.table::<MediaTransportApi>() }
            .map_err(|_| "native_layout_mismatch")?;
        accepted(unsafe { (api.drain)(self.media.context(), self.handles.media_transport) })
    }
}

fn accepted(status: Status) -> PlaneResult<bool> {
    match status {
        STATUS_OK => Ok(true),
        MEDIA_BUSY | MEDIA_PENDING => Ok(false),
        MEDIA_CLOSED => Err("native_stream_closed"),
        _ => Err("native_operation_failed"),
    }
}

fn encoder_accepted(status: Status) -> PlaneResult<bool> {
    if status == MEDIA_BACKEND_ERROR {
        Err("audio_encoder_failed")
    } else {
        accepted(status)
    }
}

struct Delivery<T> {
    result: Option<PlaneResult<T>>,
}

impl<T> Default for Delivery<T> {
    fn default() -> Self {
        Self { result: None }
    }
}

impl<T> Delivery<T> {
    fn finish(self, status: Status) -> PlaneResult<Option<T>> {
        match (status, self.result) {
            (STATUS_OK, Some(result)) => result.map(Some),
            (MEDIA_BUSY | MEDIA_PENDING, None) => Ok(None),
            (_, Some(Err(error))) => Err(error),
            (MEDIA_CLOSED, _) => Err("native_stream_closed"),
            _ => Err("native_delivery_failed"),
        }
    }
}

unsafe fn deliver<T, P>(
    context: *mut c_void,
    input: *const P,
    copy: impl FnOnce(&P) -> PlaneResult<T>,
) -> Status {
    if context.is_null() || input.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: callbacks receive exclusive stack delivery state and a live header.
    let output = unsafe { &mut *context.cast::<Delivery<T>>() };
    if output.result.is_some() {
        output.result = Some(Err("native_multiple_callbacks"));
        return STATUS_INVALID_ARGUMENT;
    }
    let result = catch_unwind(AssertUnwindSafe(|| copy(unsafe { &*input })))
        .unwrap_or(Err("native_callback_failed"));
    let status = if result.is_ok() {
        STATUS_OK
    } else {
        STATUS_INVALID_ARGUMENT
    };
    output.result = Some(result);
    status
}

unsafe extern "C" fn copy_video(context: *mut c_void, packet: *const EncodedPacket) -> Status {
    // SAFETY: trusted provider lends initialized metadata and bytes during delivery.
    unsafe { deliver(context, packet, |packet| VideoPacket::copy(packet)) }
}
unsafe extern "C" fn copy_pcm(context: *mut c_void, frame: *const AudioFrame) -> Status {
    // SAFETY: trusted provider keeps frame and samples live until callback returns.
    unsafe { deliver(context, frame, |frame| PcmPacket::copy(frame)) }
}
unsafe extern "C" fn copy_audio(context: *mut c_void, packet: *const EncodedAudioPacket) -> Status {
    // SAFETY: trusted provider keeps packet bytes initialized and immutable.
    unsafe { deliver(context, packet, |packet| AudioPacket::copy(packet)) }
}
unsafe extern "C" fn copy_control(context: *mut c_void, message: *const ControlMessage) -> Status {
    // SAFETY: provider lends authenticated plaintext for this callback only.
    unsafe {
        deliver(context, message, |message| {
            let bytes = message.bytes().map_err(|_| "invalid_control_message")?;
            Ok(ControlRecord {
                message_type: message.message_type as u16,
                payload: bytes.to_vec(),
                sequence: message.sequence,
            })
        })
    }
}

#[cfg(test)]
mod tests;
