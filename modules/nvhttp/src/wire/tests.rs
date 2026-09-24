use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct Broker {
    next: u64,
    requests: BTreeMap<u64, Value>,
    accepted: Vec<Value>,
    cancelled: BTreeSet<u64>,
    approved: bool,
    pause_begin: bool,
}

impl Broker {
    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        Ok(match command {
            Command::Request {
                service, payload, ..
            } => {
                assert_eq!(service, PAIRING_SERVICE);
                self.next += 1;
                self.accepted.push(payload.clone());
                self.requests.insert(self.next, payload);
                Response::Accepted { id: self.next }
            }
            Command::TakeResult { request } => {
                if self.cancelled.contains(&request) {
                    return Ok(Response::Error {
                        code: "cancelled".into(),
                        message: String::new(),
                    });
                }
                let payload = &self.requests[&request];
                if self.pause_begin && payload["action"] == "begin" {
                    return Ok(Response::Pending);
                }
                let result = match payload["action"].as_str().unwrap() {
                    "identity" => {
                        json!({"ok":true,"host_id":"12345678-1234-4234-8234-123456789abc"})
                    }
                    "begin" => {
                        json!({"ok":true,"pending":true,"pairing_id":"test-private-pairing-id"})
                    }
                    "get_server_cert" if !self.approved => {
                        json!({"ok":true,"pending":true,"paired":0})
                    }
                    "get_server_cert" => {
                        json!({"ok":true,"paired":1,"plaincert":hex(pem().as_bytes())})
                    }
                    "client_challenge" => {
                        json!({"ok":true,"paired":1,"challengeresponse":"00".repeat(48)})
                    }
                    "server_challenge_response" => {
                        json!({"ok":true,"paired":1,"pairingsecret":"00".repeat(272)})
                    }
                    "client_pairing_secret" => {
                        json!({"ok":true,"paired":1,"certificate_id":"00".repeat(32)})
                    }
                    "cancel" => json!({"ok":true}),
                    action => panic!("unexpected service operation {action}"),
                };
                self.requests.remove(&request);
                Response::Result { payload: result }
            }
            Command::Cancel { request } => {
                self.cancelled.insert(request);
                Response::Ok
            }
            other => panic!("unexpected runtime command {other:?}"),
        })
    }
}

fn router() -> Router {
    Router::new(
        RouterConfig {
            hostname: "Amaterasu & <test>".into(),
            http_port: 47989,
            https_port: 47984,
        },
        LaunchController::new(None).unwrap(),
    )
}

fn pem() -> String {
    pem_rfc7468::encode_string("CERTIFICATE", pem_rfc7468::LineEnding::LF, &[1, 2, 3]).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn begin_target() -> String {
    format!(
        "/pair?uniqueid=client-1&phrase=getservercert&salt={}&clientcert={}&devicename=Living+room&uuid=request-uuid",
        "00".repeat(16),
        hex(pem().as_bytes())
    )
}

fn request(target: &str) -> Request {
    Request {
        method: "GET".into(),
        target: target.into(),
        body: Vec::new(),
    }
}

fn dispatch(
    router: &mut Router,
    broker: &mut Broker,
    connection: u64,
    target: &str,
    now: Instant,
) -> Option<Reply> {
    router.dispatch_with(
        connection,
        request(target),
        peer("127.0.0.1", false, None, false),
        now,
        &mut |command| broker.send(command),
    )
}

fn poll(router: &mut Router, broker: &mut Broker, now: Instant) -> Vec<(u64, Reply)> {
    router.poll_with(now, &mut |command| broker.send(command))
}

fn ready(router: &mut Router, broker: &mut Broker, now: Instant) {
    broker.approved = true;
    assert!(dispatch(router, broker, 1, &begin_target(), now).is_none());
    for _ in 0..3 {
        assert!(poll(router, broker, now).is_empty());
    }
    let result = poll(router, broker, now);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].1.status, 200);
}

#[test]
fn query_rejects_duplicate_escaped_fields_and_bounded_malformed_input() {
    for target in [
        "/pair?uniqueid=a&unique%69d=b",
        "/pair?uniqueid=a&phase=%",
        "/pair?uniqueid=%00",
        "/pair?devicename=%C2%85",
        "/pair?uniqueid=%GG",
        "/pair?uniqueid=%ff",
        "/pair?missingequals",
        "https://attacker/pair?uniqueid=a",
    ] {
        assert!(Query::parse(target).is_err(), "{target}");
    }
    assert!(Query::parse(&format!("/pair?x={}", "a".repeat(query::MAX_TARGET))).is_err());
    let fields = (0..25)
        .map(|index| format!("x{index}=y"))
        .collect::<Vec<_>>()
        .join("&");
    assert!(Query::parse(&format!("/pair?{fields}")).is_err());
    let parsed = Query::parse("/pair?devicename=Living+room%26TV&uniqueid=client").unwrap();
    assert_eq!(parsed.get("devicename"), Some("Living room&TV"));
}

#[test]
fn xml_escapes_metadata_and_advertises_configured_streaming_honestly() {
    assert_eq!(xml::escape("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
    let response = xml::server_info(&router().config, "host-id", false, 0, false, false);
    let text = String::from_utf8(response.body).unwrap();
    assert!(text.contains("Amaterasu &amp; &lt;test&gt;"));
    assert!(text.contains("<appversion>7.1.0.0</appversion>"));
    assert!(text.contains("<ServerCodecModeSupport>0</ServerCodecModeSupport>"));
    assert!(text.contains("<PairStatus>0</PairStatus>"));
    assert!(text.contains("<currentgame>0</currentgame>"));
    assert!(text.contains("<StreamingSupported>0</StreamingSupported>"));
}

#[test]
fn configured_streaming_advertises_modern_version_without_fake_codec_support() {
    let response = xml::server_info(&router().config, "host-id", true, 0, true, true);
    let text = String::from_utf8(response.body).unwrap();
    assert!(text.contains("<appversion>7.1.431.0</appversion>"));
    assert!(text.contains("<StreamingSupported>1</StreamingSupported>"));
    assert!(text.contains("<currentgame>0</currentgame>"));
    assert!(text.contains("<state>AMATERASU_SERVER_BUSY</state>"));
    assert!(text.contains("<ServerCodecModeSupport>0</ServerCodecModeSupport>"));
}

#[test]
fn public_launch_requires_paired_tls_before_parsing_private_fields() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    let malformed = "/launch?appid=bad&mode=bad&rikey=bad&rikeyid=bad";
    let response = router
        .dispatch_with(
            1,
            request(malformed),
            peer("127.0.0.1", true, Some(vec![1, 2, 3]), false),
            now,
            &mut |command| broker.send(command),
        )
        .unwrap();
    assert_eq!(response.status, 401);
    assert!(broker.accepted.is_empty());

    let response = router
        .dispatch_with(
            2,
            request(malformed),
            peer("127.0.0.1", true, Some(vec![1, 2, 3]), true),
            now,
            &mut |command| broker.send(command),
        )
        .unwrap();
    assert_eq!(response.status, 400);
    assert!(broker.accepted.is_empty());
}

#[test]
fn server_certificate_request_is_held_until_pin_approval() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    assert!(dispatch(&mut router, &mut broker, 1, &begin_target(), now).is_none());
    for _ in 0..4 {
        assert!(poll(&mut router, &mut broker, now).is_empty());
    }
    assert_eq!(router.jobs.len(), 1);
    assert!(poll(&mut router, &mut broker, now + Duration::from_millis(100)).is_empty());
    broker.approved = true;
    let later = now + Duration::from_millis(300);
    assert!(poll(&mut router, &mut broker, later).is_empty());
    let replies = poll(&mut router, &mut broker, later);
    assert_eq!(replies.len(), 1);
    let text = String::from_utf8(replies[0].1.body.clone()).unwrap();
    assert!(text.contains("<paired>1</paired>"));
    assert!(text.contains("<plaincert>"));
    assert!(!text.contains("pending"));
}

#[test]
fn foreign_peer_duplicate_begin_and_unpair_cannot_consume_a_victim_transcript() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    ready(&mut router, &mut broker, now);
    let requests = broker.accepted.len();
    let challenge = format!(
        "/pair?uniqueid=client-1&clientchallenge={}",
        "00".repeat(16)
    );
    let foreign = router
        .dispatch_with(
            2,
            request(&challenge),
            peer("127.0.0.2", false, None, false),
            now,
            &mut |command| broker.send(command),
        )
        .unwrap();
    assert_eq!(foreign.status, 403);
    assert_eq!(
        dispatch(&mut router, &mut broker, 3, &begin_target(), now)
            .unwrap()
            .status,
        409
    );
    assert_eq!(
        dispatch(
            &mut router,
            &mut broker,
            4,
            "/unpair?uniqueid=client-1",
            now
        )
        .unwrap()
        .status,
        401
    );
    assert_eq!(broker.accepted.len(), requests);
    assert_eq!(router.transcripts.len(), 1);
    assert!(dispatch(&mut router, &mut broker, 5, &challenge, now).is_none());
}

#[test]
fn duplicate_or_out_of_order_phases_never_replay_crypto_calls() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    ready(&mut router, &mut broker, now);
    let wrong = format!(
        "/pair?uniqueid=client-1&serverchallengeresp={}",
        "00".repeat(32)
    );
    assert_eq!(
        dispatch(&mut router, &mut broker, 2, &wrong, now)
            .unwrap()
            .status,
        409
    );
    let challenge = format!(
        "/pair?uniqueid=client-1&clientchallenge={}",
        "00".repeat(16)
    );
    assert!(dispatch(&mut router, &mut broker, 3, &challenge, now).is_none());
    assert_eq!(
        dispatch(&mut router, &mut broker, 4, &challenge, now)
            .unwrap()
            .status,
        409
    );
    assert!(poll(&mut router, &mut broker, now).is_empty());
    assert_eq!(poll(&mut router, &mut broker, now)[0].1.status, 200);
    assert_eq!(
        dispatch(&mut router, &mut broker, 5, &challenge, now)
            .unwrap()
            .status,
        409
    );
    assert_eq!(
        broker
            .accepted
            .iter()
            .filter(|payload| payload["action"] == "client_challenge")
            .count(),
        1
    );
}

#[test]
fn cancelled_begin_is_drained_then_cancelled_by_opaque_id() {
    let mut router = router();
    let mut broker = Broker {
        pause_begin: true,
        ..Default::default()
    };
    let now = Instant::now();
    dispatch(&mut router, &mut broker, 1, &begin_target(), now);
    poll(&mut router, &mut broker, now);
    router.cancel_with(1, now, &mut |command| broker.send(command));
    assert!(poll(&mut router, &mut broker, now).is_empty());
    broker.pause_begin = false;
    for _ in 0..3 {
        assert!(poll(&mut router, &mut broker, now).is_empty());
    }
    assert!(router.transcripts.is_empty());
    assert!(
        broker
            .accepted
            .iter()
            .any(|payload| payload["action"] == "cancel"
                && payload["pairing_id"] == "test-private-pairing-id")
    );
}

#[test]
fn timeout_closes_held_pin_request_and_cancels_the_transcript() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    dispatch(&mut router, &mut broker, 1, &begin_target(), now);
    for _ in 0..4 {
        poll(&mut router, &mut broker, now);
    }
    let expired = now + PAIRING_TIMEOUT;
    let replies = poll(&mut router, &mut broker, expired);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1.status, 504);
    assert!(router.transcripts.is_empty());
    poll(&mut router, &mut broker, expired);
    poll(&mut router, &mut broker, expired);
    assert!(
        broker
            .accepted
            .iter()
            .any(|payload| payload["action"] == "cancel")
    );
}

#[test]
fn final_pairchallenge_requires_live_tls_trust_and_the_original_certificate() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    ready(&mut router, &mut broker, now);
    for (field, length) in [
        ("clientchallenge", 16),
        ("serverchallengeresp", 32),
        ("clientpairingsecret", 272),
    ] {
        let target = format!("/pair?uniqueid=client-1&{field}={}", "00".repeat(length));
        assert!(dispatch(&mut router, &mut broker, 2, &target, now).is_none());
        poll(&mut router, &mut broker, now);
        assert_eq!(poll(&mut router, &mut broker, now)[0].1.status, 200);
    }
    let target = "/pair?uniqueid=client-1&phrase=pairchallenge";
    assert_eq!(
        dispatch(&mut router, &mut broker, 3, target, now)
            .unwrap()
            .status,
        401
    );
    for (certificate, trusted, expected) in [
        (vec![1, 2, 3], false, 401),
        (vec![9], true, 403),
        (vec![1, 2, 3], true, 200),
    ] {
        let reply = router
            .dispatch_with(
                4,
                request(target),
                peer("127.0.0.1", true, Some(certificate), trusted),
                now,
                &mut |command| broker.send(command),
            )
            .unwrap();
        assert_eq!(reply.status, expected);
    }
}

fn peer(
    remote: &str,
    secure: bool,
    peer_certificate: Option<Vec<u8>>,
    peer_trusted: bool,
) -> RequestContext {
    RequestContext {
        remote: remote.parse().unwrap(),
        secure,
        peer_certificate,
        peer_trusted,
    }
}

#[test]
fn configured_hostname_replaces_xml_noncharacters_and_escapes_quotes() {
    let mut config = router().config;
    config.hostname = "A&<\"'\u{fffe}\u{ffff}Z".into();
    let reply = xml::server_info(&config, "host", false, 0, false, false);
    let body = String::from_utf8(reply.body).unwrap();
    assert!(body.contains("<hostname>A&amp;&lt;&quot;&apos;  Z</hostname>"));
    assert!(!body.contains('\u{fffe}'));
    assert!(!body.contains('\u{ffff}'));
}

#[test]
fn committed_hostname_updates_are_visible_in_live_serverinfo_without_restart() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();

    assert!(dispatch(&mut router, &mut broker, 1, "/serverinfo", now).is_none());
    assert!(poll(&mut router, &mut broker, now).is_empty());
    let initial = poll(&mut router, &mut broker, now);
    assert_eq!(initial.len(), 1);
    let initial_body = String::from_utf8(initial[0].1.body.clone()).unwrap();
    assert!(initial_body.contains("<hostname>Amaterasu &amp; &lt;test&gt;</hostname>"));

    router.set_hostname("Living Room & Games".into());
    router.set_ports(48_000, 47_995);
    assert!(dispatch(&mut router, &mut broker, 2, "/serverinfo", now).is_none());
    assert!(poll(&mut router, &mut broker, now).is_empty());
    let updated = poll(&mut router, &mut broker, now);
    assert_eq!(updated.len(), 1);
    let updated_body = String::from_utf8(updated[0].1.body.clone()).unwrap();
    assert!(updated_body.contains("<hostname>Living Room &amp; Games</hostname>"));
    assert!(updated_body.contains("<ExternalPort>48000</ExternalPort>"));
    assert!(updated_body.contains("<HttpsPort>47995</HttpsPort>"));
}

#[test]
fn cancellation_before_begin_dispatch_does_not_create_a_transcript() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    dispatch(&mut router, &mut broker, 1, &begin_target(), now);
    router.cancel_with(1, now, &mut |command| broker.send(command));
    assert!(poll(&mut router, &mut broker, now).is_empty());
    assert!(broker.accepted.is_empty());
    assert!(router.transcripts.is_empty());
}

#[test]
fn conflicting_phase_selectors_and_foreign_certificate_do_not_advance() {
    let mut router = router();
    let mut broker = Broker::default();
    let now = Instant::now();
    ready(&mut router, &mut broker, now);
    let invalid = format!(
        "/pair?uniqueid=client-1&phrase=pairchallenge&clientchallenge={}",
        "00".repeat(16)
    );
    assert_eq!(
        dispatch(&mut router, &mut broker, 2, &invalid, now)
            .unwrap()
            .status,
        400
    );
    let replacement =
        pem_rfc7468::encode_string("CERTIFICATE", pem_rfc7468::LineEnding::LF, &[9, 9, 9]).unwrap();
    let duplicate = begin_target().replace(&hex(pem().as_bytes()), &hex(replacement.as_bytes()));
    assert_eq!(
        dispatch(&mut router, &mut broker, 3, &duplicate, now)
            .unwrap()
            .status,
        409
    );
    let phase = format!(
        "/pair?uniqueid=client-1&clientchallenge={}&clientcert={}",
        "00".repeat(16),
        hex(replacement.as_bytes())
    );
    assert_eq!(
        dispatch(&mut router, &mut broker, 4, &phase, now)
            .unwrap()
            .status,
        403
    );
    assert_eq!(
        broker
            .accepted
            .iter()
            .filter(
                |payload| payload["action"] == "client_challenge" || payload["action"] == "cancel"
            )
            .count(),
        0
    );
    assert_eq!(router.transcripts.len(), 1);
}
