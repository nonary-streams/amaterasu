use super::*;
use amaterasu_module_sdk::{Command, Response};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn config() -> StreamingConfig {
    StreamingConfig {
        capture: CaptureConfig {
            capture_provider: CaptureProvider::Dxgi,
            output: "display-one".into(),
            encoder: 0,
            audio_endpoint: "default".into(),
        },
        profile: StreamProfile {
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
    }
}

fn peer(marker: u8) -> PeerIdentity {
    PeerIdentity::new("127.0.0.1".parse().unwrap(), vec![marker; 32]).unwrap()
}

fn request(key: u8) -> LaunchRequest {
    LaunchRequest {
        app_id: 1,
        width: 1280,
        height: 720,
        fps: 60,
        remote_input_key: [key; 16],
        key_id: 17,
    }
}

#[test]
fn settings_reject_unsupported_packetization_before_launch() {
    let mut invalid = config();
    invalid.profile.fec_percentage = 0;
    assert_eq!(invalid.validate().unwrap_err(), "invalid_streaming_profile");
}

#[test]
fn disabled_streaming_and_foreign_cancel_fail_without_work() {
    let now = Instant::now();
    let mut disabled = Controller::new(None).unwrap();
    assert_eq!(
        disabled.launch(1, request(1), peer(1), now).unwrap().status,
        503
    );

    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(1, request(1), peer(1), now).is_none());
    assert_eq!(controller.cancel(2, &peer(2), now).unwrap().status, 403);
}

#[test]
fn secret_key_reuse_is_global_but_key_id_reuse_is_allowed_for_fresh_sessions() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    assert!(controller.launch(1, request(1), peer(1), now).is_none());

    let mut changed = request(1);
    changed.app_id = 2;
    assert_eq!(
        controller.launch(2, changed, peer(2), now).unwrap().status,
        409
    );
}

struct Broker {
    next: u64,
    results: BTreeMap<u64, Value>,
    commands: Vec<Command>,
    application_state: &'static str,
    fail_rtsp_cleanup: bool,
    wrong_application_id: bool,
    lose_rtsp_open_once: bool,
    lost_rtsp_open: bool,
    application_operation: String,
    application_id: String,
    fail_rtsp_settlement: bool,
    fail_application_settlement: bool,
    unsettled_rtsp: BTreeSet<u64>,
    unsettled_application: BTreeSet<u64>,
    rtsp_closed: bool,
    application_stopped: bool,
    application_unhealthy: bool,
}

impl Default for Broker {
    fn default() -> Self {
        Self {
            next: 0,
            results: BTreeMap::new(),
            commands: Vec::new(),
            application_state: "active",
            fail_rtsp_cleanup: false,
            wrong_application_id: false,
            lose_rtsp_open_once: false,
            lost_rtsp_open: false,
            application_operation: String::new(),
            application_id: "app-0000000000000001".into(),
            fail_rtsp_settlement: false,
            fail_application_settlement: false,
            unsettled_rtsp: BTreeSet::new(),
            unsettled_application: BTreeSet::new(),
            rtsp_closed: false,
            application_stopped: false,
            application_unhealthy: false,
        }
    }
}

impl Broker {
    fn send(&mut self, command: Command) -> Transport {
        self.commands.push(command.clone());
        match command {
            Command::Request {
                service, payload, ..
            } => {
                if self.lose_rtsp_open_once
                    && !self.lost_rtsp_open
                    && service == "amaterasu.rtsp/1"
                    && payload["action"] == "open"
                {
                    self.lost_rtsp_open = true;
                    return Err("lost admission".into());
                }
                self.next += 1;
                let request = self.next;
                if service == "amaterasu.rtsp/1" && payload["action"] == "close" {
                    self.rtsp_closed = true;
                    self.unsettled_rtsp.insert(request);
                }
                if service == "amaterasu.launch/1" && payload["action"] == "stop" {
                    self.application_stopped = true;
                    self.unsettled_application.insert(request);
                }
                let result = self.response(&service, &payload);
                self.results.insert(request, result);
                Ok(Response::Accepted { id: request })
            }
            Command::TakeResult { request }
                if (self.fail_rtsp_settlement && self.unsettled_rtsp.contains(&request))
                    || (self.fail_application_settlement
                        && self.unsettled_application.contains(&request)) =>
            {
                Err("lost result".into())
            }
            Command::TakeResult { request } => Ok(Response::Result {
                payload: self.results.remove(&request).expect("queued result"),
            }),
            Command::Cancel { request } | Command::ReleaseResult { request }
                if (self.fail_rtsp_settlement && self.unsettled_rtsp.contains(&request))
                    || (self.fail_application_settlement
                        && self.unsettled_application.contains(&request)) =>
            {
                Ok(Response::error("unavailable", "fixture settlement failure"))
            }
            Command::Cancel { .. } | Command::ReleaseResult { .. } => Ok(Response::Ok),
            _ => Ok(Response::error("unexpected", "unexpected command")),
        }
    }

    fn response(&mut self, service: &str, payload: &Value) -> Value {
        let action = payload["action"].as_str().unwrap_or_default();
        if service == "amaterasu.launch/1" {
            if action == "launch_export" {
                self.application_operation = payload["operation_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.application_id = payload["app_id"].as_str().unwrap_or_default().to_owned();
            }
            let operation = self.application_operation.as_str();
            let app = self.application_id.as_str();
            let launch_id = if self.wrong_application_id { 99 } else { 41 };
            let state = if action == "stop" || (action == "status" && self.application_stopped) {
                "stopped"
            } else {
                self.application_state
            };
            return json!({
                "ok": true,
                "launch": {
                    "launch_id": launch_id,
                    "operation_id": operation,
                    "app_id": app,
                    "state": state,
                    "session_id": 2,
                    "process_id": 3,
                    "failure": null,
                    "cleanup_error": if self.application_unhealthy {
                        Value::String("fixture_failure".into())
                    } else {
                        Value::Null
                    },
                }
            });
        }
        let state = if action == "close" && self.fail_rtsp_cleanup {
            "cleanup_failed"
        } else if action == "close" {
            "closed"
        } else if action == "status" && self.fail_rtsp_cleanup {
            "cleanup_failed"
        } else if action == "status" && self.rtsp_closed {
            "closed"
        } else {
            "ready"
        };
        json!({
            "ok": true,
            "ticket": 71,
            "state": state,
            "local": "127.0.0.1:48010",
            "rtsp_url": "rtspenc://127.0.0.1:48010/launch-47",
            "session": 11,
            "stream_state": "prepared",
            "failure": null,
            "cleanup_error": if state == "cleanup_failed" {
                Value::String("fixture_failure".into())
            } else {
                Value::Null
            },
        })
    }

    fn actions(&self) -> Vec<(&str, &str)> {
        self.commands
            .iter()
            .filter_map(|command| match command {
                Command::Request {
                    service, payload, ..
                } => Some((
                    service.as_str(),
                    payload["action"].as_str().unwrap_or_default(),
                )),
                _ => None,
            })
            .collect()
    }
}

fn poll_until_reply(
    controller: &mut Controller,
    broker: &mut Broker,
    now: Instant,
) -> (u64, Reply) {
    for _ in 0..32 {
        let replies = controller.poll(now, &mut |command| broker.send(command));
        if let Some(reply) = replies.into_iter().next() {
            return reply;
        }
    }
    panic!("lifecycle did not produce a bounded reply")
}

fn launch_ready(controller: &mut Controller, broker: &mut Broker, now: Instant) {
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (connection, reply) = poll_until_reply(controller, broker, now);
    assert_eq!(connection, 10);
    assert_eq!(reply.status, 200);
    assert!(
        String::from_utf8(reply.body)
            .unwrap()
            .contains("rtspenc://")
    );
    controller.reply_delivered(connection, now);
    assert_eq!(controller.current_game(&peer(1)), 1);
}

#[test]
fn success_requires_active_application_and_ready_rtsp_then_retries_same_url() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);

    let retry = controller
        .launch(11, request(1), peer(1), now)
        .expect("idempotent ready reply");
    assert_eq!(retry.status, 200);
    controller.reply_delivered(11, now);
    assert_eq!(
        broker.actions()[..2],
        [
            ("amaterasu.launch/1", "launch_export"),
            ("amaterasu.rtsp/1", "open")
        ]
    );
}

#[test]
fn response_loss_before_first_flush_stops_rtsp_and_application() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (connection, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 200);
    controller.connection_closed(connection, now);
    for _ in 0..24 {
        controller.poll(now, &mut |command| broker.send(command));
    }
    let actions = broker.actions();
    assert!(actions.contains(&("amaterasu.rtsp/1", "close")));
    assert!(actions.contains(&("amaterasu.launch/1", "stop")));
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn rtsp_cleanup_failure_still_stops_application_and_retains_failure() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.fail_rtsp_cleanup = true;

    assert!(controller.cancel(20, &peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 503);
    assert!(broker.actions().contains(&("amaterasu.launch/1", "stop")));
    assert!(controller.busy(), "unconfirmed RTSP cleanup remains owned");
}

#[test]
fn malformed_or_wrong_provider_identity_never_becomes_ready() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.wrong_application_id = true;
    for _ in 0..4 {
        controller.poll(now, &mut |command| broker.send(command));
    }
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn unhealthy_application_response_adopts_the_owned_id_then_cleans_it() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        application_unhealthy: true,
        ..Broker::default()
    };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 503);
    assert!(broker.actions().contains(&("amaterasu.launch/1", "stop")));
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn lost_rtsp_admission_is_settled_before_owned_lookup_without_reopening() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker {
        lose_rtsp_open_once: true,
        ..Broker::default()
    };
    assert!(controller.launch(10, request(1), peer(1), now).is_none());
    let (_, reply) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(reply.status, 200);

    let rtsp_requests: Vec<_> = broker
        .commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| match command {
            Command::Request {
                service,
                payload,
                operation_id,
                ..
            } if service == "amaterasu.rtsp/1" => {
                Some((index, payload.clone(), operation_id.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(rtsp_requests.len(), 3);
    assert_eq!(rtsp_requests[0].1, rtsp_requests[1].1);
    assert_eq!(rtsp_requests[0].2, rtsp_requests[1].2);
    assert_eq!(rtsp_requests[2].1["action"], "lookup");
    let cancel = broker
        .commands
        .iter()
        .position(|command| matches!(command, Command::Cancel { .. }))
        .expect("cancel uncertain admission");
    let release = broker
        .commands
        .iter()
        .position(|command| matches!(command, Command::ReleaseResult { .. }))
        .expect("release uncertain admission");
    assert!(rtsp_requests[1].0 < cancel && cancel < release && release < rtsp_requests[2].0);
}

#[test]
fn natural_application_exit_closes_the_stream_and_releases_the_application() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.application_state = "stopped";
    for _ in 0..24 {
        controller.poll(now, &mut |command| broker.send(command));
    }
    let actions = broker.actions();
    assert!(actions.contains(&("amaterasu.rtsp/1", "close")));
    assert!(actions.contains(&("amaterasu.launch/1", "stop")));
    assert_eq!(controller.current_game(&peer(1)), 0);
}

#[test]
fn repeated_cancel_and_revocation_do_not_extend_cleanup_deadlines() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);

    assert!(controller.cancel(20, &peer(1), now).is_none());
    let deadline = controller.records.values().next().unwrap().deadline;
    assert_eq!(
        controller
            .cancel(21, &peer(1), now + Duration::from_secs(1))
            .unwrap()
            .status,
        409
    );
    assert_eq!(
        controller.records.values().next().unwrap().deadline,
        deadline
    );
    controller.revoke_untrusted(now + Duration::from_secs(2), |_| false);
    controller.revoke_untrusted(now + Duration::from_secs(3), |_| false);
    assert_eq!(
        controller.records.values().next().unwrap().deadline,
        deadline
    );
}

#[test]
fn dual_uncertain_cleanup_calls_are_both_retained_and_reconciled_before_success() {
    let now = Instant::now();
    let mut controller = Controller::new(Some(config())).unwrap();
    let mut broker = Broker::default();
    launch_ready(&mut controller, &mut broker, now);
    broker.fail_rtsp_settlement = true;
    broker.fail_application_settlement = true;
    assert!(controller.cancel(20, &peer(1), now).is_none());

    let (_, failed) = poll_until_reply(&mut controller, &mut broker, now);
    assert_eq!(failed.status, 503);
    let record = controller.records.values().next().unwrap();
    assert_eq!(record.uncertain.len(), 2);
    let deadline = record.deadline;

    broker.fail_rtsp_settlement = false;
    broker.fail_application_settlement = false;
    assert!(
        controller
            .cancel(21, &peer(1), now + Duration::from_secs(1))
            .is_none()
    );
    let (_, completed) =
        poll_until_reply(&mut controller, &mut broker, now + Duration::from_secs(1));
    assert_eq!(completed.status, 200);
    let record = controller.records.values().next().unwrap();
    assert!(record.uncertain.is_empty());
    assert!(record.state.terminal());
    assert!(
        record.deadline > deadline,
        "explicit retry gets one fresh budget"
    );
}
