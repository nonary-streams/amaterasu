//! Temporary, self-starting qualification fixture. Never include in production packages.
#![cfg(target_os = "linux")]

mod lifecycle;
use amaterasu_media::{
    EncodedPacket, FLAG_END_OF_ACCESS_UNIT, FLAG_KEYFRAME, MEDIA_PENDING, PacketSink, VideoApi,
};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient, abi};
use lifecycle::{Lifecycle, Outcome, Phase};
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 2_000_000;
const TARGET_AU: u32 = 8;
const VIDEO: &str = "amaterasu.video/1";

#[derive(Debug)]
struct Settings {
    output: String,
    nonce: String,
}
impl Settings {
    fn parse(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let synthetic = v["acknowledge_capture"] == "synthetic-test-only";
        if obj.len() != 6 || v["enabled"] != true || !synthetic {
            return None;
        }
        let output = v["output"].as_str()?;
        if output != "nv12-fixture-display" {
            return None;
        }
        let width = u32::try_from(v["width"].as_u64()?).ok()?;
        let height = u32::try_from(v["height"].as_u64()?).ok()?;
        if !(64..=1920).contains(&width)
            || !(48..=1080).contains(&height)
            || width % 2 != 0
            || height % 2 != 0
        {
            return None;
        }
        if width != 64 || height != 48 {
            return None;
        }
        let nonce = v["nonce"].as_str()?;
        if nonce.len() != 32
            || !nonce
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return None;
        }
        Some(Self {
            output: output.into(),
            nonce: nonce.into(),
        })
    }
}
#[derive(Default)]
struct Packets {
    bytes: Vec<u8>,
    access_units: u32,
    keyframe: bool,
    overflow: bool,
}
struct Probe {
    runtime: RuntimeClient,
    settings: Settings,
    lifecycle: Lifecycle,
    packets: Packets,
    deadline: Instant,
    cleanup_deadline: Option<Instant>,
    next_retry: Instant,
    counter: u64,
    status: Value,
    reported: bool,
}
impl ManagedModule for Probe {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("probe settings unavailable".into());
        };
        let settings = Settings::parse(&payload)
            .ok_or("probe refused: explicit bounded authorization required")?;
        let now = Instant::now();
        Ok(Self {
            runtime,
            settings,
            lifecycle: Lifecycle::new(),
            packets: Packets::default(),
            deadline: now + Duration::from_secs(20),
            cleanup_deadline: None,
            next_retry: now,
            counter: 0,
            status: Value::Null,
            reported: false,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        let now = Instant::now();
        if self.cleanup_deadline.is_none() && now >= self.deadline {
            self.begin_failure("probe deadline exceeded");
        }
        if self
            .cleanup_deadline
            .is_some_and(|deadline| now >= deadline)
            && self.lifecycle.phase != Phase::Done
        {
            if let Some(id) = pending_id(self.lifecycle.phase) {
                let _ = self.runtime.send_ephemeral(Command::Cancel { request: id });
            }
            self.lifecycle.expired();
        }
        if self.lifecycle.phase == Phase::Done {
            return self.report();
        }
        if now < self.next_retry {
            return Ok(());
        }
        match self.lifecycle.phase {
            Phase::Start => {
                let payload = json!({"action":"open","operation_id":format!("self-probe-{}",self.settings.nonce),"capture_provider":"custom","output":self.settings.output,"encoder":"software","packet_boundaries":true,"configuration":{"width":64,"height":48,"fps":30,"bitrate":1000000,"sw_preset":"superfast","sw_tune":"zerolatency"}});
                match self.send_request(payload) {
                    Ok(Response::Accepted { id }) => self.lifecycle.open_admitted(id),
                    Ok(Response::Error { code, .. }) => {
                        self.lifecycle.open_rejected();
                        self.lifecycle.failure = Some(format!("open admission rejected: {code}"));
                    }
                    _ => {
                        self.start_cleanup();
                        self.lifecycle
                            .open_uncertain(None, "open admission indeterminate");
                    }
                }
            }
            Phase::OpenWait(id) => match self.poll(id) {
                Some(Response::Pending) => (),
                Some(Response::Result { payload })
                    if payload["ok"] == true && payload["stream"].as_u64().is_some() =>
                {
                    self.lifecycle.opened(payload["stream"].as_u64().unwrap())
                }
                Some(Response::Result { payload }) if payload["ok"] == false => {
                    self.start_cleanup();
                    self.lifecycle
                        .open_uncertain(None, "open returned an error");
                }
                Some(response) if result_consumed(&response) => {
                    self.start_cleanup();
                    self.lifecycle.open_uncertain(None, "open response failed");
                }
                Some(_) | None => {
                    self.start_cleanup();
                    self.lifecycle
                        .open_uncertain(Some(id), "open response indeterminate");
                }
            },
            Phase::Collect => {
                if let Err(error) = self.collect() {
                    self.begin_failure(&error);
                } else if self.packets.access_units >= TARGET_AU && self.packets.keyframe {
                    match self
                        .send_request(json!({"action":"status","stream":self.lifecycle.stream}))
                    {
                        Ok(Response::Accepted { id }) => {
                            self.lifecycle.phase = Phase::StatusWait(id)
                        }
                        _ => self.begin_failure("status admission failed"),
                    }
                }
            }
            Phase::StatusWait(id) => match self.poll(id) {
                Some(Response::Pending) => (),
                Some(Response::Result { payload }) if payload["ok"] == true => {
                    self.status = payload;
                    self.start_cleanup();
                    self.lifecycle.phase = Phase::CloseStart;
                }
                Some(response) if result_consumed(&response) => {
                    self.begin_failure("status request failed");
                    self.lifecycle.cancelled();
                }
                _ => self.begin_failure("status request failed"),
            },
            Phase::Cancel(id) => {
                let _ = self.runtime.send_ephemeral(Command::Cancel { request: id });
                // Cancel may race a successful completion. Consume the terminal
                // response before proceeding so an unknown open can be closed.
                if let Some(response) = self.poll(id)
                    && result_consumed(&response)
                {
                    if let Response::Result { payload } = &response {
                        if payload["ok"] == true
                            && let Some(stream) = payload["stream"].as_u64()
                        {
                            self.lifecycle.lookup_active(stream);
                        } else {
                            self.lifecycle.cancelled();
                        }
                    } else {
                        self.lifecycle.cancelled();
                    }
                }
                self.pause_retry();
            }
            Phase::LookupStart | Phase::VerifyStart => {
                let verify = self.lifecycle.phase == Phase::VerifyStart;
                match self.send_request(json!({"action":"lookup","operation_id":format!("self-probe-{}",self.settings.nonce)})) {
                    Ok(Response::Accepted { id }) => self.lifecycle.lookup_admitted(id,verify),
                    _ => { self.lifecycle.lookup_failed(); self.pause_retry(); }
                }
            }
            Phase::LookupWait(id) | Phase::VerifyWait(id) => match self.poll(id) {
                Some(Response::Pending) => (),
                Some(Response::Result { payload })
                    if payload["ok"] == true && payload["stream"].as_u64().is_some() =>
                {
                    self.lifecycle
                        .lookup_active(payload["stream"].as_u64().unwrap());
                    self.pause_retry();
                }
                Some(Response::Result { payload })
                    if payload["ok"] == false && payload["code"] == "not_found" =>
                {
                    self.lifecycle.lookup_missing();
                    self.pause_retry();
                }
                Some(response) if result_consumed(&response) => {
                    self.lifecycle.lookup_failed();
                    self.pause_retry();
                }
                _ => {
                    self.begin_failure("lookup response indeterminate");
                    self.pause_retry();
                }
            },
            Phase::CloseStart => {
                let Some(stream) = self.lifecycle.stream else {
                    self.lifecycle.lookup_failed();
                    return Ok(());
                };
                match self.send_request(json!({"action":"close","stream":stream})) {
                    Ok(Response::Accepted { id }) => self.lifecycle.close_admitted(id),
                    _ => {
                        self.lifecycle.close_rejected();
                        self.pause_retry();
                    }
                }
            }
            Phase::CloseWait(id) => match self.poll(id) {
                Some(Response::Pending) => (),
                Some(response) if result_consumed(&response) => {
                    self.lifecycle.close_replied();
                    self.pause_retry();
                }
                _ => {
                    self.begin_failure("close response indeterminate");
                    self.pause_retry();
                }
            },
            Phase::Done => unreachable!(),
        }
        Ok(())
    }
}
impl Probe {
    fn start_cleanup(&mut self) {
        if self.cleanup_deadline.is_none() {
            self.cleanup_deadline = Some(Instant::now() + Duration::from_secs(10));
        }
    }
    fn begin_failure(&mut self, message: &str) {
        self.start_cleanup();
        self.lifecycle.fail(message);
    }
    fn pause_retry(&mut self) {
        self.next_retry = Instant::now() + Duration::from_millis(50);
    }
    fn send_request(&mut self, payload: Value) -> Result<Response, String> {
        self.counter = self.counter.saturating_add(1);
        self.runtime.send_ephemeral(Command::Request {
            service: VIDEO.into(),
            payload,
            timeout_ms: 1500,
            operation_id: format!("self-probe-rpc-{}-{}", self.settings.nonce, self.counter),
            parent: None,
        })
    }
    fn poll(&self, id: u64) -> Option<Response> {
        self.runtime
            .send_ephemeral(Command::TakeResult { request: id })
            .ok()
    }
    fn collect(&mut self) -> ModuleResult<()> {
        // SAFETY: the binding owns the table during the synchronous callback; no borrowed pointers escape.
        let binding = unsafe { amaterasu_module_sdk::video::bind(&self.runtime, VIDEO) }?;
        let api = unsafe { binding.table::<VideoApi>() }?;
        let sink = PacketSink {
            context: (&mut self.packets as *mut Packets).cast(),
            write: collect,
        };
        let status = unsafe {
            (api.receive)(
                api.base.context,
                self.lifecycle.stream.ok_or("missing stream")?,
                sink,
            )
        };
        if self.packets.overflow || !matches!(status, abi::STATUS_OK | MEDIA_PENDING) {
            return Err("packet receive failed or exceeded limit".into());
        }
        Ok(())
    }
    fn report(&mut self) -> ModuleResult<()> {
        if self.reported {
            return Ok(());
        }
        self.reported = true;
        if self.lifecycle.outcome == Some(Outcome::Clean) && self.lifecycle.failure.is_none() {
            let path = std::env::temp_dir().join(format!(
                "amaterasu-autonomous-video-{}.h264",
                self.settings.nonce
            ));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| e.to_string())?;
            if let Err(error) = file.write_all(&self.packets.bytes) {
                drop(file);
                let _ = std::fs::remove_file(&path);
                return Err(error.to_string());
            }
            eprintln!(
                "AUTONOMOUS_VIDEO_PROBE_RESULT {}",
                json!({"path":path.to_string_lossy(),"bytes":self.packets.bytes.len(),"access_units":self.packets.access_units,"keyframe":self.packets.keyframe,"encoder":"software","video":self.status,"cleanup":"confirmed"})
            );
        } else {
            eprintln!(
                "AUTONOMOUS_VIDEO_PROBE_FAILURE {}",
                json!({"failure":self.lifecycle.failure,"cleanup":format!("{:?}",self.lifecycle.outcome)})
            );
        }
        Ok(())
    }
}
fn result_consumed(response: &Response) -> bool {
    matches!(response, Response::Result { .. })
        || matches!(response, Response::Error { code, .. } if code == "cancelled" || code == "deadline_exceeded")
}
fn pending_id(phase: Phase) -> Option<u64> {
    match phase {
        Phase::OpenWait(id)
        | Phase::StatusWait(id)
        | Phase::Cancel(id)
        | Phase::LookupWait(id)
        | Phase::CloseWait(id)
        | Phase::VerifyWait(id) => Some(id),
        _ => None,
    }
}
unsafe extern "C" fn collect(context: *mut c_void, packet: *const EncodedPacket) -> abi::Status {
    if context.is_null() || packet.is_null() {
        return abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: context and packet remain valid only for this synchronous callback.
    let output = unsafe { &mut *context.cast::<Packets>() };
    let packet = unsafe { &*packet };
    let Ok(bytes) = (unsafe { packet.bytes() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    if bytes.len() > MAX_BYTES.saturating_sub(output.bytes.len()) {
        output.overflow = true;
        return abi::STATUS_INVALID_ARGUMENT;
    }
    output.bytes.extend_from_slice(bytes);
    output.keyframe |= packet.flags & FLAG_KEYFRAME != 0;
    if packet.flags & FLAG_END_OF_ACCESS_UNIT != 0 {
        output.access_units = output.access_units.saturating_add(1);
    }
    abi::STATUS_OK
}
amaterasu_module_sdk::export_module!(Probe);

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Value {
        json!({"enabled":true,"acknowledge_capture":"synthetic-test-only","output":"nv12-fixture-display","width":64,"height":48,"nonce":"0123456789abcdef0123456789abcdef"})
    }
    #[test]
    fn authorization_fails_closed() {
        assert!(Settings::parse(&valid()).is_some());
        for (key, value) in [
            ("enabled", json!(false)),
            ("acknowledge_capture", json!("yes")),
            ("output", json!("HDMI-A-1")),
            ("output", json!("kwin:Virtual-0")),
            ("output", json!("kwin:Virtual-01")),
            ("width", json!(1921)),
            ("height", json!(0)),
            ("acknowledge_capture", json!("owned-virtual-output-only")),
            ("nonce", json!("../../anything")),
        ] {
            let mut v = valid();
            v[key] = value;
            assert!(Settings::parse(&v).is_none(), "{key}");
        }
        let mut v = valid();
        v["unexpected"] = json!(true);
        assert!(Settings::parse(&v).is_none());
    }
    #[test]
    fn cancellation_requires_consumed_terminal_result() {
        assert!(!result_consumed(&Response::Pending));
        assert!(!result_consumed(&Response::error(
            "permission_denied",
            "denied"
        )));
        assert!(!result_consumed(&Response::error("not_found", "missing")));
        assert!(result_consumed(&Response::error("cancelled", "cancelled")));
        assert!(result_consumed(&Response::error(
            "deadline_exceeded",
            "expired"
        )));
        assert!(result_consumed(&Response::Result {
            payload: json!({"ok":true,"stream":42})
        }));
    }
}
