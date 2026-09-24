//! Development-only signed hardware qualification.  This component is present
//! only in `PackageProfile::MediaProbe`, never the production installer policy.
//! It chooses a finite physical output and its correlated H.264 MFT identity through normal managed
//! controls, then reads copied H.264 packets through the existing native video
//! API.  It returns only packet metadata and a digest, never screen pixels.
use amaterasu_media::{
    EncodedPacket, FLAG_END_OF_ACCESS_UNIT, FLAG_KEYFRAME, MEDIA_PENDING, PacketSink, VideoApi,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient, abi};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::c_void,
    time::{Duration, Instant},
};

const ENCODE: &str = "amaterasu.encode/1";
const VIDEO: &str = "amaterasu.video/1";
const REQUIRED_ACCESS_UNITS: u32 = 3;

#[derive(Clone, Copy)]
enum Stage {
    EnumerateOutputs,
    EnumerateEncoders,
    OpenVideo,
    ForceIdr,
    Collect,
    SuccessVideoStatus,
    SuccessCaptureStatus,
    FailureVideoStatus,
    FailureCaptureStatus,
    FailureEncoderStatus,
    CloseVideo,
}

pub struct HardwareVideoProbe {
    request: u64,
    pending: u64,
    stage: Stage,
    output: Option<Output>,
    provider: Provider,
    encoder: Option<Value>,
    stream: Option<u64>,
    collected: Collected,
    deadline: Instant,
    error: Option<String>,
    diagnostics: serde_json::Map<String, Value>,
    polls: u32,
    pending_reads: u32,
}

#[derive(Clone)]
struct Output {
    id: String,
    adapter_vendor_id: Option<u32>,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy)]
pub enum Provider {
    Dxgi,
    Wgc,
}

impl Provider {
    fn service(self) -> &'static str {
        match self {
            Self::Dxgi => "amaterasu.capture-dxgi/1",
            Self::Wgc => "amaterasu.capture-wgc/1",
        }
    }

    fn request_name(self) -> &'static str {
        match self {
            Self::Dxgi => "dxgi",
            Self::Wgc => "wgc",
        }
    }
}

#[derive(Default)]
struct Collected {
    access_units: u32,
    bytes: usize,
    packets: u32,
    keyframe: bool,
    annex_b: bool,
    digest: Sha256,
}

impl HardwareVideoProbe {
    pub fn start(runtime: &RuntimeClient, request: u64, provider: Provider) -> ModuleResult<Self> {
        let pending = request_service(
            runtime,
            request,
            provider.service(),
            json!({"action":"enumerate"}),
            "hardware-video-outputs",
        )?;
        Ok(Self {
            request,
            pending,
            stage: Stage::EnumerateOutputs,
            output: None,
            provider,
            encoder: None,
            stream: None,
            collected: Collected::default(),
            deadline: Instant::now() + Duration::from_secs(10),
            error: None,
            diagnostics: serde_json::Map::new(),
            polls: 0,
            pending_reads: 0,
        })
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        if matches!(self.stage, Stage::Collect) {
            return self.collect(runtime);
        }
        let payload = match runtime.send_ephemeral(Command::TakeResult {
            request: self.pending,
        })? {
            Response::Pending => return Ok(false),
            Response::Result { payload } if payload["ok"] == true => payload,
            response => {
                return self.fail(
                    runtime,
                    format!("hardware_video_control_failed:{response:?}"),
                );
            }
        };
        match self.stage {
            Stage::EnumerateOutputs => self.select_output(runtime, payload),
            Stage::EnumerateEncoders => self.select_encoder(runtime, payload),
            Stage::OpenVideo => self.force_idr(runtime, payload),
            Stage::ForceIdr => {
                self.stage = Stage::Collect;
                Ok(false)
            }
            Stage::SuccessVideoStatus => {
                self.diagnostics.insert("video".into(), payload);
                self.pending = request_service(
                    runtime,
                    self.request,
                    self.provider.service(),
                    json!({"action":"status"}),
                    "hardware-capture-success-status",
                )?;
                self.stage = Stage::SuccessCaptureStatus;
                Ok(false)
            }
            Stage::SuccessCaptureStatus => {
                self.diagnostics.insert("capture".into(), payload);
                self.close(runtime, None)
            }
            Stage::FailureVideoStatus => self.capture_diagnostics(runtime, payload),
            Stage::FailureCaptureStatus => self.encoder_diagnostics(runtime, payload),
            Stage::FailureEncoderStatus => {
                self.diagnostics.insert("encoder".into(), payload);
                self.close(runtime, None)
            }
            Stage::CloseVideo => self.complete(runtime),
            Stage::Collect => unreachable!("collect does not await a control result"),
        }
    }

    fn select_output(&mut self, runtime: &RuntimeClient, payload: Value) -> ModuleResult<bool> {
        let Some(output) = payload["outputs"]
            .as_array()
            .and_then(|outputs| outputs.iter().find_map(parse_output))
        else {
            return self.fail(runtime, "no_usable_dxgi_output".into());
        };
        self.diagnostics
            .insert("selected_capture_output".into(), payload.clone());
        self.output = Some(output);
        self.pending = request_service(
            runtime,
            self.request,
            ENCODE,
            json!({"action":"enumerate"}),
            "hardware-video-encoders",
        )?;
        self.stage = Stage::EnumerateEncoders;
        Ok(false)
    }

    fn select_encoder(&mut self, runtime: &RuntimeClient, payload: Value) -> ModuleResult<bool> {
        let output = self
            .output
            .as_ref()
            .expect("output selected before encoder");
        let Some((encoder, selection)) = select_h264(&payload, output.adapter_vendor_id) else {
            return self.fail(runtime, "no_h264_mft_for_capture_adapter".into());
        };
        self.diagnostics
            .insert("encoder_selection".into(), selection);
        self.encoder = Some(encoder.clone());
        self.pending = request_service(
            runtime,
            self.request,
            VIDEO,
            json!({
                "action":"open",
                "packet_boundaries":true,
                "capture_provider":self.provider.request_name(),
                "output":output.id,
                "encoder":encoder,
                "configuration":{"width":output.width,"height":output.height,"fps":30,"bitrate":2_000_000},
                "operation_id":format!("hardware-video-{}", self.request),
            }),
            "hardware-video-open",
        )?;
        self.stage = Stage::OpenVideo;
        Ok(false)
    }

    fn force_idr(&mut self, runtime: &RuntimeClient, payload: Value) -> ModuleResult<bool> {
        let Some(stream) = payload["stream"].as_u64() else {
            return self.fail(runtime, "hardware_video_open_missing_stream".into());
        };
        self.stream = Some(stream);
        self.pending = request_service(
            runtime,
            self.request,
            VIDEO,
            json!({"action":"force_idr","stream":stream}),
            "hardware-video-force-idr",
        )?;
        self.stage = Stage::ForceIdr;
        Ok(false)
    }

    fn collect(&mut self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        if Instant::now() >= self.deadline {
            return self.begin_diagnostics(runtime, "hardware_video_timeout");
        }
        let stream = self.stream.expect("video stream exists while collecting");
        let binding = unsafe { runtime.bind_native(VIDEO, 1, 0, size_of::<VideoApi>() as u32) }?;
        let api = unsafe { binding.table::<VideoApi>() }?;
        let sink = PacketSink {
            context: (&mut self.collected as *mut Collected).cast(),
            write: collect_packet,
        };
        let status = unsafe { (api.receive)(api.base.context, stream, sink) };
        self.polls = self.polls.saturating_add(1);
        if !matches!(status, abi::STATUS_OK | MEDIA_PENDING) {
            return self
                .begin_diagnostics(runtime, &format!("hardware_video_receive_status_{status}"));
        }
        if status == MEDIA_PENDING {
            self.pending_reads = self.pending_reads.saturating_add(1);
        }
        if self.collected.access_units >= REQUIRED_ACCESS_UNITS
            && self.collected.keyframe
            && self.collected.annex_b
        {
            self.pending = request_service(
                runtime,
                self.request,
                VIDEO,
                json!({"action":"status","stream":stream}),
                "hardware-video-success-status",
            )?;
            self.stage = Stage::SuccessVideoStatus;
            return Ok(false);
        }
        Ok(false)
    }

    fn begin_diagnostics(&mut self, runtime: &RuntimeClient, error: &str) -> ModuleResult<bool> {
        self.error = Some(error.into());
        self.pending = request_service(
            runtime,
            self.request,
            VIDEO,
            json!({"action":"status","stream":self.stream}),
            "hardware-video-status",
        )?;
        self.stage = Stage::FailureVideoStatus;
        Ok(false)
    }

    fn capture_diagnostics(
        &mut self,
        runtime: &RuntimeClient,
        payload: Value,
    ) -> ModuleResult<bool> {
        if self.error.as_deref() == Some("hardware_video_timeout") {
            self.error = Some(timeout_classification(&payload).into());
        }
        self.diagnostics.insert("video".into(), payload);
        self.pending = request_service(
            runtime,
            self.request,
            self.provider.service(),
            json!({"action":"status"}),
            "hardware-capture-status",
        )?;
        self.stage = Stage::FailureCaptureStatus;
        Ok(false)
    }

    fn encoder_diagnostics(
        &mut self,
        runtime: &RuntimeClient,
        payload: Value,
    ) -> ModuleResult<bool> {
        self.diagnostics.insert("capture".into(), payload);
        self.pending = request_service(
            runtime,
            self.request,
            ENCODE,
            json!({"action":"enumerate"}),
            "hardware-encoder-status",
        )?;
        self.stage = Stage::FailureEncoderStatus;
        Ok(false)
    }

    fn close(&mut self, runtime: &RuntimeClient, error: Option<&str>) -> ModuleResult<bool> {
        let stream = self.stream.expect("video stream exists while closing");
        self.pending = request_service(
            runtime,
            self.request,
            VIDEO,
            json!({"action":"close","stream":stream}),
            "hardware-video-close",
        )?;
        self.stage = Stage::CloseVideo;
        if let Some(error) = error {
            self.error = Some(error.into());
        }
        Ok(false)
    }

    fn complete(&self, runtime: &RuntimeClient) -> ModuleResult<bool> {
        let digest = self.collected.digest.clone().finalize();
        let complete = runtime.send_ephemeral(Command::Complete {
            request: self.request,
            payload: json!({
                "ok": self.error.is_none() && self.collected.access_units >= REQUIRED_ACCESS_UNITS && self.collected.keyframe && self.collected.annex_b,
                "code": self.error,
                "provider": self.provider.request_name(),
                "polls": self.polls,
                "pending_reads": self.pending_reads,
                "diagnostics": self.diagnostics,
                "output": self.output.as_ref().map(|output| json!({
                    "id": output.id,
                    "left": output.left,
                    "top": output.top,
                    "width": output.width,
                    "height": output.height,
                })),
                "encoder": self.encoder,
                "access_units": self.collected.access_units,
                "packets": self.collected.packets,
                "bytes": self.collected.bytes,
                "keyframe": self.collected.keyframe,
                "annex_b": self.collected.annex_b,
                "sha256": format!("{digest:x}"),
            }),
        })?;
        Ok(matches!(complete, Response::Ok | Response::Error { .. }))
    }

    fn fail(&mut self, runtime: &RuntimeClient, code: String) -> ModuleResult<bool> {
        let complete = runtime.send_ephemeral(Command::Complete {
            request: self.request,
            payload: json!({"ok":false,"code":code}),
        })?;
        Ok(matches!(complete, Response::Ok | Response::Error { .. }))
    }
}

fn request_service(
    runtime: &RuntimeClient,
    parent: u64,
    service: &str,
    payload: Value,
    operation: &str,
) -> ModuleResult<u64> {
    match runtime.send_ephemeral(Command::Request {
        service: service.into(),
        payload,
        timeout_ms: 5000,
        operation_id: format!("{operation}-{parent}"),
        parent: Some(parent),
    })? {
        Response::Accepted { id } => Ok(id),
        response => Err(format!("hardware_video_request_rejected:{response:?}")),
    }
}

fn parse_output(value: &Value) -> Option<Output> {
    let width = value["width"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())?;
    let height = value["height"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())?;
    let id = value["id"].as_str()?;
    let adapter_vendor_id = value["adapter_vendor_id"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok());
    let left = value["left"]
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())?;
    let top = value["top"]
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())?;
    (value["attached"] == true
        && (16..=3840).contains(&width)
        && (16..=2160).contains(&height)
        && width.is_multiple_of(2)
        && height.is_multiple_of(2))
    .then(|| Output {
        id: id.into(),
        adapter_vendor_id,
        left,
        top,
        width,
        height,
    })
}

fn select_h264(payload: &Value, capture_vendor: Option<u32>) -> Option<(Value, Value)> {
    let encoders = payload["encoders"].as_array()?;
    let usable = encoders.iter().filter(|encoder| {
        encoder["codec"] == "h264"
            && encoder["hardware"] == true
            && valid_encoder_identity(&encoder["identity"])
    });
    let vendor = capture_vendor?;
    let encoder = usable
        .clone()
        .find(|encoder| encoder["vendor_id"].as_u64() == Some(u64::from(vendor)))
        .or_else(|| {
            vendor_name(vendor).and_then(|name| {
                usable.clone().find(|encoder| {
                    encoder["name"]
                        .as_str()
                        .is_some_and(|value| value.to_ascii_lowercase().contains(name))
                })
            })
        })?;
    let method = if encoder["vendor_id"].as_u64() == Some(u64::from(vendor)) {
        "mft_vendor_attribute"
    } else {
        "friendly_name_fallback"
    };
    Some((
        encoder["identity"].clone(),
        json!({"capture_vendor_id":vendor,"method":method,"identity":encoder["identity"]}),
    ))
}

fn valid_encoder_identity(value: &Value) -> bool {
    value["clsid"]
        .as_str()
        .is_some_and(|value| !value.is_empty())
        && value["friendly_name"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        && (value["hardware_url"].is_null() || value["hardware_url"].is_string())
}

fn timeout_classification(video: &Value) -> &'static str {
    if video["submitted_frames"].as_u64() == Some(0) && video["error"].is_null() {
        "hardware_video_static_capture_timeout"
    } else {
        "hardware_video_encoder_stall"
    }
}

fn vendor_name(vendor: u32) -> Option<&'static str> {
    match vendor {
        0x10de => Some("nvidia"),
        0x8086 => Some("intel"),
        _ => None,
    }
}

unsafe extern "C" fn collect_packet(
    context: *mut c_void,
    packet: *const EncodedPacket,
) -> abi::Status {
    if context.is_null() || packet.is_null() {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the provider borrows the packet through this callback; the fixture
    // only hashes it synchronously and retains no screen/video content.
    let collected = unsafe { &mut *context.cast::<Collected>() };
    let packet = unsafe { &*packet };
    let Ok(bytes) = (unsafe { packet.bytes() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    collected.digest.update(bytes);
    collected.bytes = collected.bytes.saturating_add(bytes.len());
    collected.packets = collected.packets.saturating_add(1);
    collected.keyframe |= packet.flags & FLAG_KEYFRAME != 0;
    collected.annex_b |= bytes.windows(3).any(|window| window == [0, 0, 1])
        || bytes.windows(4).any(|window| window == [0, 0, 0, 1]);
    if packet.flags & FLAG_END_OF_ACCESS_UNIT != 0 {
        collected.access_units = collected.access_units.saturating_add(1);
    }
    abi::STATUS_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_attached_output_and_prefers_its_h264_vendor_capability() {
        let output =
            json!({"id":"display","attached":true,"left":0,"top":0,"width":1920,"height":1080});
        assert_eq!(parse_output(&output).unwrap().id, "display");
        assert!(
            parse_output(
                &json!({"id":"odd","attached":true,"left":0,"top":0,"width":1919,"height":1080})
            )
            .is_none()
        );
        assert!(
            parse_output(
                &json!({"id":"off","attached":false,"left":0,"top":0,"width":1920,"height":1080})
            )
            .is_none()
        );
        let (identity, selection) = select_h264(
            &json!({"encoders":[
                {"name":"NVIDIA H.264","codec":"h264","hardware":true,"vendor_id":4318,"identity":{"clsid":"nvidia","hardware_url":"nvidia://0","friendly_name":"NVIDIA H.264"}},
                {"name":"Intel(R) Quick Sync H.264","codec":"h264","hardware":true,"vendor_id":32902,"identity":{"clsid":"intel","hardware_url":"intel://0","friendly_name":"Intel H.264"}}
            ]}),
            Some(0x10de),
        )
        .unwrap();
        assert_eq!(identity["clsid"], "nvidia");
        assert_eq!(selection["method"], "mft_vendor_attribute");
    }

    #[test]
    fn refuses_implicit_encoder_when_capture_adapter_is_unknown() {
        assert!(select_h264(
            &json!({"encoders":[{"name":"NVIDIA H.264","codec":"h264","hardware":true,"identity":{"clsid":"nvidia","hardware_url":null,"friendly_name":"NVIDIA H.264"}}]}),
            None,
        )
        .is_none());
    }

    #[test]
    fn distinguishes_static_capture_from_encoder_stall() {
        assert_eq!(
            timeout_classification(&json!({"submitted_frames":0,"error":null})),
            "hardware_video_static_capture_timeout"
        );
        assert_eq!(
            timeout_classification(&json!({"submitted_frames":1,"error":null})),
            "hardware_video_encoder_stall"
        );
    }
}
