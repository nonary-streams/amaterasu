use super::*;
use amaterasu_gamestream::crypto::{Direction, RtspOpener, RtspSealer};
use amaterasu_module_sdk::stream_lifecycle::{IssueDisposition, StreamReport};
use std::io::{Read, Write};

const KEY: [u8; 16] = [47; 16];

fn prepared_controller() -> (Controller, u64) {
    let (mut record, _) = announced_profile_record();
    record.owner = "amaterasu.nvhttp".into();
    record.config.operation_id = "policy-launch".into();
    record.config.profile.encrypt_video = false;
    record.config.profile.encrypt_audio = false;
    let (negotiation, action) = pending_play(&record);
    record.negotiation = Some(negotiation);
    let mut controller = Controller::new(Settings::default()).unwrap();
    let mut transport = Transport::bind("127.0.0.1:0".parse().unwrap(), false).unwrap();
    transport
        .add_ticket(record.id, "127.0.0.1".parse().unwrap(), &KEY)
        .unwrap();
    controller.transport = Some(transport);
    controller.records.insert(record.id, record);
    (controller, action)
}

/// Exercise the controller and authenticated transport on an ephemeral loopback
/// socket, including response completion, which must not revoke the ticket.
fn exchange(
    controller: &mut Controller,
    sealer: &mut RtspSealer,
    opener: &mut RtspOpener,
    plaintext: &str,
) -> String {
    let address = controller.transport.as_ref().unwrap().local_addr().unwrap();
    let mut client = TcpStream::connect(address).unwrap();
    client
        .write_all(&sealer.seal(plaintext.as_bytes()).unwrap())
        .unwrap();
    client.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut received = Vec::new();
    loop {
        controller.poll_transport(10);
        let mut buffer = [0; 4096];
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => received.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("response read: {error}"),
        }
        assert!(Instant::now() < deadline, "bounded loopback exchange");
        thread::sleep(Duration::from_millis(1));
    }
    String::from_utf8(opener.open(&received).unwrap().plaintext).unwrap()
}

fn request_text(method: &str, cseq: u32, session: &str) -> String {
    format!("{method} / RTSP/1.0\r\nCSeq: {cseq}\r\nSession: {session}\r\n\r\n")
}

fn assert_retained(controller: &Controller, state: State) {
    let record = &controller.records[&5];
    assert_eq!(record.state, state);
    assert_eq!(record.failure, None);
    assert_eq!(record.revoke_after, None);
    assert_eq!(record.session, Some(23));
    assert!(
        controller.job.is_none(),
        "refusal must not schedule native work"
    );
}

#[test]
fn request_state_and_sequence_refusals_preserve_the_stream_and_allow_cleanup() {
    let (mut controller, action) = prepared_controller();
    let mut sealer = RtspSealer::new(&KEY, Direction::ClientToHost).unwrap();
    let mut opener = RtspOpener::new(&KEY, Direction::HostToClient).unwrap();
    let session = controller.records[&5].session_token.clone();
    let response = exchange(
        &mut controller,
        &mut sealer,
        &mut opener,
        &request_text("PLAY", 8, &session),
    );
    assert!(response.starts_with("RTSP/1.0 455 "));
    assert_retained(&controller, State::Ready);
    {
        let record = controller.records.get_mut(&5).unwrap();
        record
            .negotiation
            .as_mut()
            .unwrap()
            .complete(5, action)
            .unwrap();
        record.state = State::Playing;
    }
    for (method, cseq, status) in [("PLAY", 7, 400), ("OPTIONS", 8, 455)] {
        let response = exchange(
            &mut controller,
            &mut sealer,
            &mut opener,
            &request_text(method, cseq, &session),
        );
        assert!(response.starts_with(&format!("RTSP/1.0 {status} ")));
        assert_retained(&controller, State::Playing);
    }
    let reports: Vec<_> = controller
        .records
        .get_mut(&5)
        .unwrap()
        .issues
        .drain()
        .collect();
    let issues: Vec<_> = reports
        .iter()
        .filter_map(|report| match report {
            StreamReport::Issue(issue) => Some(issue),
            _ => None,
        })
        .collect();
    assert_eq!(
        issues
            .iter()
            .map(|issue| issue.reason.as_str())
            .collect::<Vec<_>>(),
        ["pending", "invalid_sequence", "wrong_state"]
    );
    let mut lifecycle = amaterasu_module_sdk::stream_lifecycle::StreamLifecycleStore::new();
    lifecycle.open("policy-launch");
    for issue in issues {
        lifecycle.commit("amaterasu.rtsp", issue.clone());
        assert_eq!(issue.operation, "policy-launch");
        assert_eq!(issue.code, "negotiation_request_rejected");
        assert_eq!(issue.disposition, IssueDisposition::AdvisoryWarning);
        assert!(issue.valid());
    }
    assert!(lifecycle.stream("policy-launch").unwrap().healthy());
    // Refused requests did not advance CSeq or revoke the authenticated ticket.
    let record = controller.records.get_mut(&5).unwrap();
    let step = record
        .negotiation
        .as_mut()
        .unwrap()
        .handle(5, &request(Method::Teardown, "/", 8, true))
        .unwrap();
    assert!(matches!(
        step,
        NegotiationStep::Action {
            action: Action::Teardown,
            ..
        }
    ));
}

#[test]
fn session_authentication_failure_still_rejects_and_retires_the_ticket() {
    let (mut controller, action) = prepared_controller();
    let record = controller.records.get_mut(&5).unwrap();
    record
        .negotiation
        .as_mut()
        .unwrap()
        .complete(5, action)
        .unwrap();
    record.state = State::Playing;
    let mut sealer = RtspSealer::new(&KEY, Direction::ClientToHost).unwrap();
    let mut opener = RtspOpener::new(&KEY, Direction::HostToClient).unwrap();
    let response = exchange(
        &mut controller,
        &mut sealer,
        &mut opener,
        &request_text("PLAY", 8, "other-session"),
    );
    assert!(response.starts_with("RTSP/1.0 454 "));
    let record = controller.records.get_mut(&5).unwrap();
    assert_eq!(record.state, State::Closing);
    assert_eq!(record.failure, Some("wrong_session"));
    let reports: Vec<_> = record.issues.drain().collect();
    assert!(reports.iter().any(|report| matches!(report,
        StreamReport::Issue(issue) if issue.is_critical() && issue.code == "wrong_session")));
}

#[test]
fn undeliverable_recoverable_refusal_preserves_the_stream() {
    let (mut controller, _) = prepared_controller();
    controller.handle_network(999, 5, request(Method::Play, "/", 8, true), 10);
    assert_retained(&controller, State::Ready);
}

#[test]
fn undeliverable_native_completion_retains_critical_rollback() {
    let (mut controller, action) = prepared_controller();
    let mut record = controller.records.remove(&5).unwrap();
    assert!(!controller.finish_network_result(
        &mut record,
        999,
        action,
        Ok(super::super::network::NetworkCompletion::NativeStart),
        None
    ));
    assert_eq!(record.state, State::Closing);
    assert_eq!(record.failure, Some("response_unavailable"));
}

#[test]
fn wrong_state_before_options_allows_a_corrected_request_on_the_same_ticket() {
    let (mut controller, _) = prepared_controller();
    let record = controller.records.get_mut(&5).unwrap();
    record.negotiation = Some(
        Negotiation::new(
            TrustedLaunch {
                epoch: record.id,
                rtsp_target: record.url.clone(),
                session_token: record.session_token.clone(),
                encrypted_rtsp: true,
                audio_port: 49_000,
                video_port: 49_002,
                control_port: 49_004,
                control_connect_data: record.config.connect_data,
                udp_discovery: None,
            },
            record.capabilities(),
        )
        .unwrap(),
    );
    let target = record.url.clone();
    let mut sealer = RtspSealer::new(&KEY, Direction::ClientToHost).unwrap();
    let mut opener = RtspOpener::new(&KEY, Direction::HostToClient).unwrap();
    let response = exchange(
        &mut controller,
        &mut sealer,
        &mut opener,
        "PLAY / RTSP/1.0\r\nCSeq: 1\r\n\r\n",
    );
    assert!(response.starts_with("RTSP/1.0 455 "));
    assert_retained(&controller, State::Ready);
    let response = exchange(
        &mut controller,
        &mut sealer,
        &mut opener,
        &format!("OPTIONS {target} RTSP/1.0\r\nCSeq: 1\r\n\r\n"),
    );
    assert!(response.starts_with("RTSP/1.0 200 "));
    assert_retained(&controller, State::Ready);
    assert_eq!(
        controller.records[&5].negotiation.as_ref().unwrap().state(),
        amaterasu_gamestream::rtsp::negotiation::State::Describe
    );
}

#[test]
fn noncritical_reports_cannot_close_or_fence_a_playing_rtsp_ticket() {
    use amaterasu_module_sdk::stream_lifecycle::StreamFailure;
    let (mut controller, action) = prepared_controller();
    let record = controller.records.get_mut(&5).unwrap();
    record
        .negotiation
        .as_mut()
        .unwrap()
        .complete(5, action)
        .unwrap();
    record.state = State::Playing;
    record.close(Some(StreamFailure::advisory("bitrate_hint")));
    record.fail_cleanup(StreamFailure::demoted("optional_unavailable", "continue"));
    assert_eq!(record.state, State::Playing);
    assert_eq!(record.failure, None);
    assert_eq!(record.cleanup_error, None);
    assert_eq!(record.session, Some(23));
    record.close(Some(StreamFailure::critical("authorization_lost")));
    assert_eq!(record.state, State::Closing);
    assert_eq!(record.failure, Some("authorization_lost"));
}

#[test]
fn provider_report_classification_survives_rpc_to_the_final_wire_boundary() {
    use amaterasu_module_sdk::stream_lifecycle::{LifecycleLayer, StreamIssue};
    for scenario in [
        "warning",
        "critical",
        "wrong_scope",
        "local_warning",
        "local_critical",
        "wrong_local_id",
        "wrong_local_layer",
        "wrong_local_phase",
        "local_with_history",
        "local_external",
        "local_missing_session",
        "local_wrong_reply_session",
        "local_cleanup_error",
    ] {
        let continues = matches!(scenario, "warning" | "local_warning");
        let (mut controller, action) = prepared_controller();
        let mut record = controller.records.remove(&5).unwrap();
        record.state = State::Arming;
        record.config.history_activity_id = None;
        if scenario == "local_with_history" {
            record.config.history_activity_id = Some(record.config.operation_id.clone());
        }
        let prepare = record.config.prepare(&record.operation(), None);
        if matches!(scenario, "local_warning" | "local_critical") {
            assert!(prepare["history_activity_id"].is_null());
            assert!(
                prepare["rtsp_operation_id"].is_null(),
                "exercise the real managed no-history payload"
            );
        }
        if scenario == "local_external" {
            record.config.lifecycle_capture_mode =
                amaterasu_extension_api::CaptureBindingMode::External;
        }
        let operation = match scenario {
            "wrong_scope" => "other-stream".into(),
            "wrong_local_id" => "ss-18".into(),
            "warning" | "critical" => record.config.operation_id.clone(),
            _ => "ss-17".into(),
        };
        let layer = if scenario == "wrong_local_layer" {
            LifecycleLayer::Input
        } else {
            LifecycleLayer::StreamSession
        };
        let phase = if scenario == "wrong_local_phase" {
            "cleanup"
        } else {
            "start"
        };
        let report = if matches!(scenario, "critical" | "local_critical") {
            StreamIssue::critical(operation, layer, phase, "required_resource_failed")
                .because("native_failure")
        } else {
            StreamIssue::advisory(operation, layer, phase, "request_adjustment")
                .because("retry_prepared_request")
        };
        let mut sealer = RtspSealer::new(&KEY, Direction::ClientToHost).unwrap();
        let mut client =
            TcpStream::connect(controller.transport.as_ref().unwrap().local_addr().unwrap())
                .unwrap();
        client
            .write_all(
                &sealer
                    .seal(request_text("PLAY", 7, &record.session_token).as_bytes())
                    .unwrap(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let connection = loop {
            let events = controller.transport.as_mut().unwrap().poll(10);
            if let Some(Event::Request { connection, .. }) = events.into_iter().next() {
                break connection;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        };
        let response_session = if scenario == "local_wrong_reply_session" {
            Some(24)
        } else if scenario == "local_missing_session" {
            record.session = None;
            None
        } else {
            Some(23)
        };
        let cleanup_error = if scenario == "local_cleanup_error" {
            Some("native_cleanup_unconfirmed")
        } else {
            None
        };
        controller.armed(
            &mut record,
            Job {
                ticket: 5,
                phase: Phase::Arm,
                call: Call::for_service(
                    "amaterasu.stream-session/1",
                    json!({}),
                    "test-arm".into(),
                    None,
                ),
                caller: None,
                network: Some((connection, action)),
                deadline: 1000,
            },
            &json!({"ok":false,"session":response_session,"state":"prepared","cleanup_error":cleanup_error,
                "code":report.code,"stream_failure":report}),
            10,
        );
        if continues {
            assert_eq!(record.state, State::Ready);
            assert_eq!(record.failure, None);
            assert_eq!(record.session, Some(23));
        } else {
            assert_eq!(record.state, State::Closing);
        }
        controller.records.insert(5, record);
        client.set_nonblocking(true).unwrap();
        let mut bytes = Vec::new();
        loop {
            controller.poll_transport(11);
            let mut buffer = [0; 4096];
            match client.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let reply = RtspOpener::new(&KEY, Direction::HostToClient)
            .unwrap()
            .open(&bytes)
            .unwrap()
            .plaintext;
        let expected = if continues {
            b"RTSP/1.0 455 ".as_slice()
        } else {
            b"RTSP/1.0 503 ".as_slice()
        };
        assert!(
            reply.starts_with(expected),
            "{scenario}: {}",
            String::from_utf8_lossy(&reply)
        );
        if continues {
            assert_retained(&controller, State::Ready);
        }
    }
}

#[test]
fn failed_connection_of_a_scoped_refusal_does_not_retire_the_ticket() {
    let (mut controller, _) = prepared_controller();
    controller.noncritical_connections.insert(42);
    controller.disconnected(42, Some(5), "peer_disconnected");
    assert_retained(&controller, State::Ready);
    assert!(controller.noncritical_connections.is_empty());
    controller.mark_noncritical_connection(999);
    assert!(
        controller.noncritical_connections.is_empty(),
        "nonexistent connections cannot grow the set"
    );
    controller.disconnected(43, Some(5), "peer_disconnected");
    assert_eq!(controller.records[&5].state, State::Closing);
    assert_eq!(controller.records[&5].failure, Some("network_disconnected"));
}

#[test]
fn undeliverable_negotiation_only_completion_preserves_ownership() {
    let (mut controller, action) = prepared_controller();
    let mut record = controller.records.remove(&5).unwrap();
    assert!(!controller.finish_network_result(
        &mut record,
        999,
        action,
        Ok(super::super::network::NetworkCompletion::NegotiationOnly),
        None
    ));
    assert_eq!(record.state, State::Ready);
    assert_eq!(record.failure, None);
    assert_eq!(record.session, Some(23));
}

#[test]
fn aborted_queued_requests_do_not_become_stream_transport_failures() {
    for reason in [
        "peer_closed",
        "tcp_read_failed",
        "connection_deadline",
        "unexpected_pipelined_data",
    ] {
        let (mut controller, _) = prepared_controller();
        controller
            .pending_network
            .push_back(super::super::PendingNetwork {
                connection: 81,
                ticket: 5,
                request: request(Method::Play, "/", 8, true),
            });
        controller.disconnected(81, Some(5), reason);
        assert!(controller.pending_network.is_empty());
        if reason == "unexpected_pipelined_data" {
            assert_eq!(
                controller.records[&5].state,
                State::Closing,
                "invalid required framing remains a critical protocol failure"
            );
        } else {
            assert_retained(&controller, State::Ready);
        }
    }
}
