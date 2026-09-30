//! Integration fixture: binds media tables and exercises software Opus without hardware.
mod control;
mod hardware;
mod opus;
mod pipewire;
mod transport;

use amaterasu_media::{
    AudioCaptureApi, AudioEncoderApi, AudioFrame, AudioPacketSink, AudioSink,
    CAPTURE_INTERFACE_MINOR, CaptureApi, CaptureApiPrefix, CaptureGeometry, CpuFrame,
    EncodedAudioPacket, EncodedPacket, EncoderApi, FrameSink, PacketSink, VideoApi,
};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient, abi};
use std::ffi::c_void;

struct Probe {
    runtime: RuntimeClient,
    opus: Option<opus::OpusProbe>,
    control: Option<control::ControlProbe>,
    hardware: Option<hardware::HardwareVideoProbe>,
    pipewire: Option<pipewire::CaptureProbe>,
    transport: Option<transport::TransportProbe>,
}

impl ManagedModule for Probe {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: "test.media-probe/1".into(),
        })?;
        Ok(Self {
            runtime,
            opus: None,
            control: None,
            hardware: None,
            pipewire: None,
            transport: None,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(probe) = &mut self.control {
            if probe.tick(&self.runtime)? {
                self.control = None;
            }
            return Ok(());
        }
        if let Some(probe) = &mut self.hardware {
            if probe.tick(&self.runtime)? {
                self.hardware = None;
            }
            return Ok(());
        }
        if let Some(probe) = &mut self.pipewire {
            if probe.tick(&self.runtime)? {
                self.pipewire = None;
            }
            return Ok(());
        }
        if let Some(probe) = &mut self.transport {
            if probe.tick(&self.runtime)? {
                self.transport = None;
            }
            return Ok(());
        }
        if let Some(probe) = &mut self.opus {
            if probe.tick(&self.runtime)? {
                self.opus = None;
            }
            return Ok(());
        }
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => Ok(()),
            Response::Request { id, payload, .. } => {
                if !matches!(
                    self.runtime.send(Command::CheckRequest { request: id })?,
                    Response::Ok
                ) {
                    return Ok(());
                }
                if matches!(
                    payload["action"].as_str(),
                    Some("control_open" | "control_close")
                ) {
                    self.control = Some(control::ControlProbe::start(&self.runtime, id, payload)?);
                    return Ok(());
                }
                if payload["action"] == "control_echo" {
                    let stream = payload["stream"].as_u64().ok_or("missing control stream")?;
                    let payload = control::echo(&self.runtime, stream)?;
                    return self.runtime.require_success(Command::Complete {
                        request: id,
                        payload,
                    });
                }
                if payload["action"] == "transport_udp" {
                    self.transport = Some(transport::TransportProbe::start(
                        &self.runtime,
                        id,
                        payload,
                    )?);
                    return Ok(());
                }
                if payload["action"] == "pipewire_capture" {
                    let source_type = payload["source_type"].as_str().unwrap_or("monitor");
                    self.pipewire = Some(pipewire::CaptureProbe::start(
                        &self.runtime,
                        id,
                        source_type,
                    )?);
                    return Ok(());
                }
                let provider = match payload["action"].as_str() {
                    Some("hardware_video") => Some(hardware::Provider::Dxgi),
                    Some("hardware_video_wgc") => Some(hardware::Provider::Wgc),
                    _ => None,
                };
                if let Some(provider) = provider {
                    self.hardware = Some(hardware::HardwareVideoProbe::start(
                        &self.runtime,
                        id,
                        provider,
                    )?);
                    return Ok(());
                }
                if payload == serde_json::json!({"action":"encode_opus"}) {
                    self.opus = Some(opus::OpusProbe::start(&self.runtime, id)?);
                    return Ok(());
                }
                let payload = self.inspect()?;
                self.runtime.require_success(Command::Complete {
                    request: id,
                    payload,
                })
            }
            _ => Err("media probe polling failed".into()),
        }
    }
}

impl Probe {
    fn inspect(&self) -> ModuleResult<serde_json::Value> {
        let mut callbacks = 0u32;
        let context = (&mut callbacks as *mut u32).cast();
        let frame_sink = FrameSink {
            context,
            write: unexpected_frame,
        };
        let packet_sink = PacketSink {
            context,
            write: unexpected_packet,
        };
        let audio_sink = AudioSink {
            context,
            write: unexpected_audio,
        };
        let audio_packet_sink = AudioPacketSink {
            context,
            write: unexpected_audio_packet,
        };

        // SAFETY: signed dependencies keep these exact media-v1 providers alive.
        // Every call is synchronous and receives the same live test-owned counter.
        // An absent stream must never invoke any sink.
        let (capture, wgc, encoder, video, audio, audio_encoder) = unsafe {
            (
                self.inspect_interface::<CaptureApiPrefix>("amaterasu.capture-dxgi/1", |api| {
                    (api.read_frame)(api.base.context, u64::MAX, frame_sink)
                })?,
                self.inspect_interface::<CaptureApiPrefix>("amaterasu.capture-wgc/1", |api| {
                    (api.read_frame)(api.base.context, u64::MAX, frame_sink)
                })?,
                self.inspect_interface::<EncoderApi>("amaterasu.encode/1", |api| {
                    (api.receive)(api.base.context, u64::MAX, packet_sink)
                })?,
                self.inspect_interface::<VideoApi>("amaterasu.video/1", |api| {
                    (api.receive)(api.base.context, u64::MAX, packet_sink)
                })?,
                self.inspect_interface::<AudioCaptureApi>("amaterasu.audio/1", |api| {
                    (api.read_audio)(api.base.context, u64::MAX, audio_sink)
                })?,
                self.inspect_interface::<AudioEncoderApi>("amaterasu.audio-encode/1", |api| {
                    (api.receive)(api.base.context, u64::MAX, audio_packet_sink)
                })?,
            )
        };
        Ok(serde_json::json!({
            "ok": true,
            "capture_status": capture,
            "wgc_status": wgc,
            "encoder_status": encoder,
            "video_status": video,
            "audio_status": audio,
            "audio_encoder_status": audio_encoder,
            "capture_geometry_status": self.inspect_capture_geometry("amaterasu.capture-dxgi/1")?,
            "wgc_geometry_status": self.inspect_capture_geometry("amaterasu.capture-wgc/1")?,
            "callbacks": callbacks,
        }))
    }

    fn inspect_capture_geometry(&self, service: &str) -> ModuleResult<abi::Status> {
        // SAFETY: the signed capture dependency supplies the v1.1 table. The
        // binding keeps it alive, and this synchronous call retains no pointers.
        let binding = unsafe {
            self.runtime.bind_native(
                service,
                1,
                CAPTURE_INTERFACE_MINOR,
                size_of::<CaptureApi>() as u32,
            )
        }?;
        // SAFETY: the successful query required the full v1.1 table layout.
        let api = unsafe { binding.table::<CaptureApi>() }?;
        let mut geometry = std::mem::MaybeUninit::<CaptureGeometry>::uninit();
        // SAFETY: output storage is writable and lives through the call. An
        // absent stream should return CLOSED; no output is read on any status.
        Ok(unsafe { (api.geometry)(api.base.context, u64::MAX, geometry.as_mut_ptr()) })
    }

    /// # Safety
    /// T is the exact C table promised by this signed media-v1 service. Provider
    /// and runtime outlive this call; the inspector obeys the product ABI and
    /// retains no table, callbacks or borrowed buffers after returning.
    unsafe fn inspect_interface<T>(
        &self,
        service: &str,
        inspect: impl FnOnce(&T) -> abi::Status,
    ) -> ModuleResult<abi::Status> {
        // SAFETY: exact layout, dependency and borrowing guarantees come from caller.
        let binding = unsafe {
            self.runtime
                .bind_native(service, 1, 0, size_of::<T>() as u32)
        }?;
        let table = unsafe { binding.table::<T>() }?;
        Ok(inspect(table))
    }
}

unsafe extern "C" fn unexpected_frame(context: *mut c_void, _: *const CpuFrame) -> abi::Status {
    // SAFETY: fixture supplies this exclusive counter only during the synchronous call.
    unsafe {
        *context.cast::<u32>() += 1;
    }
    abi::STATUS_OK
}
unsafe extern "C" fn unexpected_packet(
    context: *mut c_void,
    _: *const EncodedPacket,
) -> abi::Status {
    // SAFETY: same synchronous test-owned counter as the frame callback.
    unsafe {
        *context.cast::<u32>() += 1;
    }
    abi::STATUS_OK
}
unsafe extern "C" fn unexpected_audio(context: *mut c_void, _: *const AudioFrame) -> abi::Status {
    // SAFETY: fixture supplies this exclusive counter for one synchronous call.
    unsafe {
        *context.cast::<u32>() += 1;
    }
    abi::STATUS_OK
}

unsafe extern "C" fn unexpected_audio_packet(
    context: *mut c_void,
    _: *const EncodedAudioPacket,
) -> abi::Status {
    // SAFETY: fixture supplies this exclusive counter for one synchronous call.
    unsafe {
        *context.cast::<u32>() += 1;
    }
    abi::STATUS_OK
}

amaterasu_module_sdk::export_module!(Probe);
