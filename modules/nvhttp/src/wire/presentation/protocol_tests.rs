//! Raw Gen7 requests exercise the real router, policy adapter and controller.
//! Native owners are stateful broker fixtures; no sockets or displays are opened.
use super::*;
use crate::launch::{
    CaptureConfig, CaptureProvider, DesktopSettings, StreamProfile, StreamingConfig,
};
use crate::wire::{Request as HttpRequest, Router};

const EPOCH: &str = "11111111111111111111111111111111";

#[derive(Clone, Copy)]
enum PolicyDecision {
    Auxiliary,
    ResumeAuxiliary,
    Desktop,
}

struct Broker {
    next: u64,
    results: BTreeMap<u64, Value>,
    calls: Vec<(String, String, Value)>,
    sessions: BTreeMap<u64, Value>,
    close_requested: BTreeSet<u64>,
    close_results: BTreeSet<u64>,
    hold_close: bool,
    decision: PolicyDecision,
}

impl Broker {
    fn new(decision: PolicyDecision) -> Self {
        Self {
            next: 0,
            results: BTreeMap::new(),
            calls: vec![],
            sessions: BTreeMap::new(),
            close_requested: BTreeSet::new(),
            close_results: BTreeSet::new(),
            hold_close: false,
            decision,
        }
    }

    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        Ok(match command {
            Command::CallApi {
                service,
                method,
                payload,
                ..
            } => {
                assert_eq!(service, session::SERVICE);
                self.calls.push((service, method.clone(), payload.clone()));
                let result = self.policy(&method, &payload);
                self.admit(result)
            }
            Command::Request {
                service, payload, ..
            } => {
                let action = payload["action"].as_str().unwrap().to_owned();
                self.calls
                    .push((service.clone(), action.clone(), payload.clone()));
                let result = self.owner(&service, &action, &payload);
                let response = self.admit(result);
                if service == "amaterasu.rtsp/1" && action == "close" {
                    self.close_results.insert(self.next);
                }
                response
            }
            Command::TakeResult { request }
                if self.hold_close && self.close_results.contains(&request) =>
            {
                Response::Pending
            }
            Command::TakeResult { request } => Response::Result {
                payload: self
                    .results
                    .remove(&request)
                    .expect("one terminal result per admission"),
            },
            Command::ReleaseResult { .. } => Response::Ok,
            Command::Cancel { .. } => Response::Ok,
            other => panic!("unexpected raw protocol broker command: {other:?}"),
        })
    }

    fn admit(&mut self, payload: Value) -> Response {
        self.next += 1;
        self.results.insert(self.next, payload);
        Response::Accepted { id: self.next }
    }

    fn policy(&self, method: &str, payload: &Value) -> Value {
        match method {
            "recovery_work" => json!(session::Reply::RecoveryWork {
                items: vec![],
                next_cursor: None
            }),
            "begin_project" => json!(session::Reply::Started {
                projection_id: "raw-http-projection".into()
            }),
            "append_project" => json!(session::Reply::Appended {
                complete: payload["final_page"] == true
            }),
            "release_project" | "cancel_dispatch" | "bind_transport" | "bind_application"
            | "observe" => json!(session::Reply::Acknowledged {}),
            "dispatch" => {
                let decision = match self.decision {
                    PolicyDecision::Desktop if payload["intent"] == "resume" => {
                        Decision::ResumeApplication {
                            application_id: crate::launch::DESKTOP_APP_ID.to_string(),
                            source: None,
                        }
                    }
                    PolicyDecision::Desktop => Decision::LaunchApplication {
                        application_id: crate::launch::DESKTOP_APP_ID.to_string(),
                        source: None,
                    },
                    PolicyDecision::Auxiliary | PolicyDecision::ResumeAuxiliary => {
                        let plan = session::AuxiliaryStreamPlan {
                            session_id: "retained-http-source".into(),
                            generation: 7,
                            reservation_id: payload["operation_id"].as_str().unwrap().into(),
                            capture: session::CaptureSource::SyntheticBlack,
                            source_mode: None,
                            mode: serde_json::from_value(payload["mode"].clone()).unwrap(),
                            stream_audio: false,
                            host_audio: false,
                        };
                        if matches!(self.decision, PolicyDecision::ResumeAuxiliary) {
                            Decision::ResumeAuxiliary { plan }
                        } else {
                            Decision::Auxiliary { plan }
                        }
                    }
                };
                json!(session::Reply::Dispatched { decision })
            }
            _ => panic!("unexpected raw protocol policy method: {method}"),
        }
    }

    fn owner(&mut self, service: &str, action: &str, payload: &Value) -> Value {
        match (service, action) {
            ("amaterasu.pairing/1", "check_client_certificate") => json!({
                "ok":true,"paired":1,"client_id":"paired-http-client",
                "certificate_id":fingerprint(&[1,2,3]),"config_overrides":{},
            }),
            ("amaterasu.library/1", "identity_inventory") => {
                json!({"ok":true,"revision":7,"entries":[],"next_cursor":null})
            }
            ("amaterasu.input/1", "settings_get") => {
                json!({"ok":true,"persisted":true,"revision":1,"values":{"proton_dualsense_compatibility":false}})
            }
            ("amaterasu.stream-session/1", "reserve_history_activity") => {
                json!({"ok":true,"reserved":true,
                "activity_id":payload["activity_id"],"rtsp_operation_id":payload["rtsp_operation_id"]})
            }
            ("amaterasu.stream-session/1", "release_history_activity") => {
                json!({"ok":true,"released":true,
                "activity_id":payload["activity_id"],"rtsp_operation_id":payload["rtsp_operation_id"]})
            }
            ("amaterasu.rtsp/1", "retirement_status") => json!({"ok":true,"runtime_epoch":EPOCH,
                "startup_reconciled":true,"operation_id":null,"expected_runtime_epoch":null,
                "state":"ready","retired":false,"cleanup_pending":false,"pending_admissions":0,"active_operations":0}),
            ("amaterasu.rtsp/1", "open") => {
                let ticket = self.sessions.len() as u64 + 1;
                let mut session = payload.clone();
                session["ok"] = json!(true);
                session["ticket"] = json!(ticket);
                session["encoder"] = payload["capture"]["encoder"].clone();
                session["rtsp_url"] = json!("rtspenc://127.0.0.1:48010");
                session["state"] = json!("ready");
                session["failure"] = Value::Null;
                session["cleanup_error"] = Value::Null;
                if session.get("display_request").is_none() {
                    session["display_request"] = Value::Null;
                }
                if session.get("display_policy_snapshot").is_none() {
                    session["display_policy_snapshot"] = Value::Null;
                }
                self.sessions.insert(ticket, session.clone());
                session
            }
            ("amaterasu.rtsp/1", "close" | "status") => {
                let ticket = payload["ticket"].as_u64().unwrap();
                if action == "close" {
                    self.close_requested.insert(ticket);
                }
                let mut session = self.sessions[&ticket].clone();
                if self.close_requested.contains(&ticket) {
                    session["state"] = json!("closed");
                }
                session
            }
            _ => panic!("unexpected raw protocol owner operation: {service} {action}"),
        }
    }

    fn open_count(&self) -> usize {
        self.calls
            .iter()
            .filter(|(service, action, _)| service == "amaterasu.rtsp/1" && action == "open")
            .count()
    }
}

fn router() -> Router {
    let mut launches = Controller::new(Some(StreamingConfig {
        capture: CaptureConfig {
            capture_provider: CaptureProvider::Dxgi,
            output: "display-one".into(),
            encoder: amaterasu_module_sdk::EncoderSelection::Index(0),
            audio_endpoint: "default".into(),
        },
        desktop: DesktopSettings { enabled: true },
        profile: StreamProfile {
            codec: amaterasu_media::VideoCodec::H264,
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
            bitrate_kbps: 10_000,
            max_reference_frames: 1,
            packet_size: 1024,
            fec_percentage: 20,
            min_parity_shards: 2,
            max_fec_blocks: 1,
            packet_ms: 10,
            audio_bitrate: 128_000,
            encrypt_video: true,
            encrypt_audio: true,
            permit_legacy_audio_iv_wrap: false,
        },
    }))
    .unwrap();
    launches.set_borrowed_capture_verification(true);
    launches.set_auxiliary_capture_routes(Some(CaptureProvider::External("black".into())), None);
    let mut router = Router::new(
        RouterConfig {
            hostname: "raw-http".into(),
            http_port: 47989,
            https_port: 47984,
        },
        launches,
    );
    router.presentation = Some(PresentationRouter::new().unwrap());
    router
}

fn peer() -> RequestContext {
    RequestContext {
        secure: true,
        remote: "127.0.0.1".parse().unwrap(),
        peer_certificate: Some(vec![1, 2, 3]),
        peer_trusted: true,
        list_allowed: true,
        launch_allowed: true,
        view_allowed: true,
        terminate_allowed: true,
    }
}

fn target(route: &str, key: u8) -> String {
    format!(
        "{route}mode=16x16x60&rikey={}&rikeyid=17",
        format!("{key:02x}").repeat(16)
    )
}

fn dispatch(router: &mut Router, connection: u64, target: &str, now: Instant) -> Option<Reply> {
    router.dispatch_with(
        connection,
        HttpRequest {
            method: "GET".into(),
            target: target.into(),
            body: vec![],
        },
        peer(),
        now,
        &mut |_| panic!("request dispatch is asynchronous"),
    )
}

fn complete(router: &mut Router, broker: &mut Broker, now: Instant) -> (u64, Reply) {
    for tick in 0..300 {
        if let Some(reply) = router
            .poll_with_identity(
                now + Duration::from_millis(tick * 20),
                &mut |command| broker.send(command),
                |_, _| true,
            )
            .into_iter()
            .next()
        {
            return reply;
        }
    }
    panic!("raw protocol stream did not complete");
}

fn assert_stream_xml(reply: &Reply, key: &str) {
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let body = String::from_utf8_lossy(&reply.body);
    assert!(body.contains("status_code=\"200\""), "{body}");
    assert!(body.contains(&format!("<{key}>1</{key}>")), "{body}");
    let other = if key == "resume" {
        "gamesession"
    } else {
        "resume"
    };
    assert!(!body.contains(&format!("<{other}>")), "{body}");
    assert!(
        body.contains("<sessionUrl0>rtspenc://127.0.0.1:48010"),
        "{body}"
    );
}

#[test]
fn raw_resume_without_appid_and_applist_launch_use_their_own_xml_keys_for_the_same_auxiliary_decision()
 {
    for decision in [PolicyDecision::Auxiliary, PolicyDecision::ResumeAuxiliary] {
        for (route, key) in [("/resume?", "resume"), ("/launch?appid=42&", "gamesession")] {
            let mut router = router();
            let mut broker = Broker::new(decision);
            let now = Instant::now();
            assert!(dispatch(&mut router, 1, &target(route, 1), now).is_none());
            let (_, reply) = complete(&mut router, &mut broker, now);
            assert_stream_xml(&reply, key);
            let request = &broker
                .calls
                .iter()
                .find(|(_, method, _)| method == "dispatch")
                .unwrap()
                .2;
            assert_eq!(request["selection"].is_null(), route == "/resume?");
        }
    }
}

#[test]
fn raw_cached_auxiliary_retries_keep_the_existing_ticket_and_follow_the_latest_http_route() {
    for (first, second, key) in [
        ("/launch?appid=42&", "/resume?", "resume"),
        ("/resume?", "/launch?appid=42&", "gamesession"),
    ] {
        let mut router = router();
        let mut broker = Broker::new(PolicyDecision::ResumeAuxiliary);
        let now = Instant::now();
        assert!(dispatch(&mut router, 1, &target(first, 1), now).is_none());
        assert_eq!(complete(&mut router, &mut broker, now).1.status, 200);
        router.reply_delivered(1);
        assert!(dispatch(&mut router, 2, &target(second, 1), now).is_none());
        let (_, reply) = complete(&mut router, &mut broker, now);
        assert_stream_xml(&reply, key);
        assert_eq!(broker.open_count(), 1);
        assert!(
            broker
                .calls
                .iter()
                .any(|(_, method, _)| method == "cancel_dispatch")
        );
    }
}

#[test]
fn raw_auxiliary_reconnect_waits_for_retirement_and_keeps_the_requested_xml_key() {
    for (first, second, key) in [
        ("/launch?appid=42&", "/resume?", "resume"),
        ("/resume?", "/launch?appid=42&", "gamesession"),
    ] {
        let mut router = router();
        let mut broker = Broker::new(PolicyDecision::ResumeAuxiliary);
        let now = Instant::now();
        assert!(dispatch(&mut router, 1, &target(first, 1), now).is_none());
        assert_eq!(complete(&mut router, &mut broker, now).1.status, 200);
        router.reply_delivered(1);
        broker.hold_close = true;
        assert!(dispatch(&mut router, 2, &target(second, 2), now).is_none());
        for tick in 0..60 {
            assert!(
                router
                    .poll_with_identity(
                        now + Duration::from_millis(tick * 20),
                        &mut |command| broker.send(command),
                        |_, _| true
                    )
                    .is_empty()
            );
        }
        assert_eq!(broker.open_count(), 1);
        assert!(!broker.close_requested.is_empty());
        broker.hold_close = false;
        let (_, reply) = complete(&mut router, &mut broker, now + Duration::from_secs(2));
        assert_stream_xml(&reply, key);
        assert_eq!(broker.open_count(), 2);
    }
}

#[test]
fn raw_resume_rejects_explicit_invalid_selection_and_launch_still_requires_one() {
    for route in [
        "/launch?",
        "/launch?appid=0&",
        "/resume?appid=0&",
        "/resume?appid=&",
        "/resume?appid=invalid&",
    ] {
        let mut router = router();
        let now = Instant::now();
        let reply = dispatch(&mut router, 1, &target(route, 1), now).unwrap();
        assert_eq!(reply.status, 400, "route {route}");
        assert!(router.presentation.as_ref().unwrap().jobs.is_empty());
    }
}

#[test]
fn raw_ordinary_resume_without_appid_is_resolved_to_the_trusted_current_application() {
    let mut router = router();
    let mut broker = Broker::new(PolicyDecision::Desktop);
    let now = Instant::now();
    assert!(
        dispatch(
            &mut router,
            1,
            &target(
                &format!("/launch?appid={}&", crate::launch::DESKTOP_APP_ID),
                1
            ),
            now
        )
        .is_none()
    );
    assert_eq!(complete(&mut router, &mut broker, now).1.status, 200);
    router.reply_delivered(1);
    assert!(dispatch(&mut router, 2, &target("/resume?", 2), now).is_none());
    let (_, reply) = complete(&mut router, &mut broker, now + Duration::from_secs(1));
    assert_stream_xml(&reply, "resume");
    let request = &broker
        .calls
        .iter()
        .rev()
        .find(|(_, method, _)| method == "dispatch")
        .unwrap()
        .2;
    assert!(request["selection"].is_null());
    assert_eq!(
        request["context"]["game"]["application"]["id"],
        crate::launch::DESKTOP_APP_ID.to_string()
    );
}

#[test]
fn an_explicitly_disabled_synthetic_route_rejects_raw_auxiliary_resume_before_transport_effects() {
    let mut router = router();
    router.set_auxiliary_black_capture_route_key(None);
    let mut broker = Broker::new(PolicyDecision::ResumeAuxiliary);
    let now = Instant::now();
    assert!(dispatch(&mut router, 1, &target("/resume?", 1), now).is_none());
    let (_, reply) = complete(&mut router, &mut broker, now);
    assert_eq!(reply.status, 503);
    assert_eq!(broker.open_count(), 0);
}
