use super::*;
use crate::approval::tests::approval;
use amaterasu_vkms_frame_helper::protocol::{
    FrameDescriptor, HELPER_ADMIN_PERMITTED, encode_request, encode_response,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

fn request(id: u32, request: Request) -> Vec<u8> {
    encode_request(&RequestRecord {
        request_id: id,
        request,
    })
    .to_vec()
}

#[test]
fn only_exact_approved_card_can_open_and_frames_require_open() {
    let mut policy = SessionPolicy::default();
    assert!(
        policy
            .admit(&request(1, Request::Frame { crtc_id: 1 }), 3)
            .is_err()
    );
    assert!(
        policy
            .admit(
                &request(
                    1,
                    Request::OpenCard {
                        major: 226,
                        minor: 2
                    }
                ),
                3
            )
            .is_err()
    );
    assert!(
        policy
            .admit(&request(1, Request::OpenCard { major: 1, minor: 3 }), 3)
            .is_err()
    );
    let open = policy
        .admit(
            &request(
                1,
                Request::OpenCard {
                    major: 226,
                    minor: 3,
                },
            ),
            3,
        )
        .unwrap();
    policy
        .accept_reply(
            &open,
            &ResponseRecord {
                request_id: 1,
                opcode: 2,
                reply: Reply::CardOpened,
            },
            0,
        )
        .unwrap();
    assert!(
        policy
            .admit(&request(2, Request::Frame { crtc_id: 1 }), 3)
            .is_ok()
    );
    assert!(
        policy
            .admit(
                &request(
                    3,
                    Request::OpenCard {
                        major: 226,
                        minor: 3
                    }
                ),
                3
            )
            .is_err()
    );
}

#[test]
fn zero_replayed_out_of_order_and_malformed_requests_are_refused() {
    let mut policy = SessionPolicy::default();
    assert!(policy.admit(&request(0, Request::Hello), 0).is_err());
    policy.admit(&request(1, Request::Hello), 0).unwrap();
    assert!(policy.admit(&request(3, Request::Hello), 0).is_err());
    assert!(policy.admit(&request(1, Request::Hello), 0).is_err());
    assert!(policy.admit(&[0; protocol::REQUEST_LEN], 0).is_err());
}

#[test]
fn request_sequence_wraps_like_the_helper_client_without_accepting_zero() {
    let mut policy = SessionPolicy {
        last_request_id: u32::MAX - 1,
        card_open: false,
    };
    policy.admit(&request(u32::MAX, Request::Hello), 0).unwrap();
    assert!(policy.admit(&request(0, Request::Hello), 0).is_err());
    policy.admit(&request(1, Request::Hello), 0).unwrap();
    assert!(policy.admit(&request(u32::MAX, Request::Hello), 0).is_err());
}

#[test]
fn mismatched_replies_and_unexpected_descriptors_are_refused() {
    let mut policy = SessionPolicy::default();
    let hello = policy.admit(&request(1, Request::Hello), 0).unwrap();
    let mut response = ResponseRecord {
        request_id: 2,
        opcode: 1,
        reply: Reply::Hello {
            capabilities: HELPER_ADMIN_PERMITTED,
        },
    };
    assert!(policy.accept_reply(&hello, &response, 0).is_err());
    response.request_id = 1;
    assert!(policy.accept_reply(&hello, &response, 1).is_err());
    response.opcode = 3;
    assert!(policy.accept_reply(&hello, &response, 0).is_err());
}

#[test]
fn proxy_forwards_verified_records_and_shuts_down_after_worker_closes() {
    let (worker, proxy_client) = SeqpacketChannel::pair().unwrap();
    let (proxy_helper, helper) = SeqpacketChannel::pair().unwrap();
    let server = std::thread::spawn(move || {
        proxy_session(&proxy_client, &proxy_helper, &approval(), || Ok(()))
    });
    worker.send(&request(1, Request::Hello), &[]).unwrap();
    let mut request_buffer = [0; protocol::REQUEST_LEN + 1];
    let received = helper.receive(&mut request_buffer).unwrap();
    assert_eq!(
        protocol::decode_request(&request_buffer[..received.len])
            .unwrap()
            .request,
        Request::Hello
    );
    let expected = encode_response(&ResponseRecord {
        request_id: 1,
        opcode: 1,
        reply: Reply::Hello {
            capabilities: HELPER_ADMIN_PERMITTED,
        },
    });
    helper.send(&expected, &[]).unwrap();
    let mut response_buffer = [0; protocol::RESPONSE_LEN + 1];
    let received = worker.receive(&mut response_buffer).unwrap();
    assert_eq!(&response_buffer[..received.len], expected.as_slice());
    drop(worker);
    assert!(server.join().unwrap().is_ok());
}

#[test]
fn revocation_after_driver_response_prevents_descriptor_delivery() {
    let (worker, proxy_client) = SeqpacketChannel::pair().unwrap();
    let (proxy_helper, helper) = SeqpacketChannel::pair().unwrap();
    let approved = Arc::new(AtomicBool::new(true));
    let (ready_sender, ready_receiver) = mpsc::channel();
    let (resume_sender, resume_receiver) = mpsc::channel();
    let authorization = GatedAuthorization {
        approved: approved.clone(),
        executed: AtomicUsize::new(0),
        response_ready: ready_sender,
        resume: Mutex::new(resume_receiver),
    };
    let server = std::thread::spawn(move || {
        proxy_session(&proxy_client, &proxy_helper, &approval(), authorization)
    });
    worker
        .send(
            &request(
                1,
                Request::OpenCard {
                    major: 226,
                    minor: 0,
                },
            ),
            &[],
        )
        .unwrap();
    let mut buffer = [0; protocol::RESPONSE_LEN + 1];
    helper.receive(&mut buffer).unwrap();
    helper
        .send(
            &encode_response(&ResponseRecord {
                request_id: 1,
                opcode: 2,
                reply: Reply::CardOpened,
            }),
            &[],
        )
        .unwrap();
    worker.receive(&mut buffer).unwrap();
    worker
        .send(&request(2, Request::Frame { crtc_id: 1 }), &[])
        .unwrap();
    helper.receive(&mut buffer).unwrap();
    let frame = FrameDescriptor {
        crtc_id: 1,
        sequence: 1,
        timestamp_ns: 1,
        width: 1,
        height: 1,
        fourcc: 1,
        modifier: 0,
        plane_count: 1,
        pitches: [4, 0, 0, 0],
        offsets: [0; 4],
        fence_mask: 0,
        guard_count: 0,
        color: 0,
        hdr_metadata: [0; 12],
    };
    let descriptor = File::open("/dev/null").unwrap();
    helper
        .send(
            &encode_response(&ResponseRecord {
                request_id: 2,
                opcode: 3,
                reply: Reply::FrameReady(frame),
            }),
            &[descriptor.as_fd()],
        )
        .unwrap();
    // Pause at the final authorization boundary after the driver has returned.
    // Revocation wins before the handoff enters its protected operation.
    ready_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    approved.store(false, Ordering::SeqCst);
    resume_sender.send(()).unwrap();
    assert!(server.join().unwrap().is_err());
    let received = worker.receive(&mut buffer).unwrap();
    assert!(received.is_end_of_stream());
    assert!(received.descriptors.is_empty());
}

struct GatedAuthorization {
    approved: Arc<AtomicBool>,
    executed: AtomicUsize,
    response_ready: mpsc::Sender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}

impl SessionAuthorization for GatedAuthorization {
    fn check(&self) -> Result<(), String> {
        self.approved
            .load(Ordering::SeqCst)
            .then_some(())
            .ok_or("revoked".into())
    }

    fn execute(&self, operation: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        // OPEN request/reply and FRAME request are the first three operations.
        if self.executed.fetch_add(1, Ordering::SeqCst) == 3 {
            self.response_ready
                .send(())
                .map_err(|error| error.to_string())?;
            self.resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .map_err(|error| error.to_string())?;
        }
        self.check()?;
        operation()
    }
}

#[test]
fn attached_request_descriptors_never_reach_the_helper() {
    let (worker, proxy_client) = SeqpacketChannel::pair().unwrap();
    let (proxy_helper, helper) = SeqpacketChannel::pair().unwrap();
    let server = std::thread::spawn(move || {
        proxy_session(&proxy_client, &proxy_helper, &approval(), || Ok(()))
    });
    let descriptor = File::open("/dev/null").unwrap();
    worker
        .send(&request(1, Request::Hello), &[descriptor.as_fd()])
        .unwrap();
    assert!(server.join().unwrap().is_err());
    let mut buffer = [0; protocol::REQUEST_LEN + 1];
    assert!(helper.receive(&mut buffer).unwrap().is_end_of_stream());
}

#[test]
fn closing_worker_interrupts_an_outstanding_helper_response() {
    let (worker, proxy_client) = SeqpacketChannel::pair().unwrap();
    let (proxy_helper, helper) = SeqpacketChannel::pair().unwrap();
    let (finished, completion) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let result = proxy_session(&proxy_client, &proxy_helper, &approval(), || Ok(()));
        finished.send(result).unwrap();
    });
    worker.send(&request(1, Request::Hello), &[]).unwrap();
    helper.receive(&mut [0; protocol::REQUEST_LEN + 1]).unwrap();
    worker.shutdown_handle().unwrap().shutdown();
    let timely = completion.recv_timeout(Duration::from_millis(250));
    // Release the fixture even when the assertion will fail.
    drop(helper);
    server.join().unwrap();
    assert!(
        timely.is_ok(),
        "worker closure was ignored while awaiting helper reply"
    );
}

#[test]
fn independent_retirement_receipt_is_sent_only_after_reaper_finishes() {
    let (receipt, authority) = SeqpacketChannel::pair().unwrap();
    let (entered, began) = mpsc::channel();
    let (release, held) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        finish_retirement(Some(&authority), || {
            entered.send(()).unwrap();
            held.recv().unwrap();
            Ok(())
        })
    });
    began.recv().unwrap();
    assert!(
        receipt.receive_nonblocking(&mut [0; 9]).is_err(),
        "held reaper must not acknowledge retirement"
    );
    release.send(()).unwrap();
    thread.join().unwrap().unwrap();
    let mut bytes = [0; 9];
    let response = receipt.receive(&mut bytes).unwrap();
    assert_eq!(&bytes[..response.len], AUTHORITY_SESSION_RETIRED);
    assert!(response.descriptors.is_empty());
}

#[test]
fn failed_reaper_never_sends_retired_receipt() {
    let (receipt, authority) = SeqpacketChannel::pair().unwrap();
    assert_eq!(
        finish_retirement(Some(&authority), || Err("wait failed".into())).unwrap_err(),
        "wait failed"
    );
    let mut bytes = [0; 9];
    let response = receipt.receive(&mut bytes).unwrap();
    assert_eq!(&bytes[..response.len], AUTHORITY_RETIREMENT_FAILED);
    assert!(receipt.receive_nonblocking(&mut bytes).is_err());
}

#[test]
fn lost_v2_handoff_never_reaches_helper_allocation() {
    let (client, authority) = SeqpacketChannel::pair().unwrap();
    client.shutdown_handle().unwrap().shutdown();
    assert!(!await_initial_hello(&authority, || Ok(())).unwrap());
}

#[test]
fn v2_allocates_only_after_exact_initial_hello_and_rechecks_authority() {
    let (client, authority) = SeqpacketChannel::pair().unwrap();
    client.send(&request(1, Request::Hello), &[]).unwrap();
    let checks = AtomicUsize::new(0);
    assert!(
        await_initial_hello(&authority, || {
            checks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap()
    );
    assert_eq!(checks.load(Ordering::SeqCst), 2);
    let mut bytes = [0; protocol::REQUEST_LEN + 1];
    let queued = authority.receive(&mut bytes).unwrap();
    assert_eq!(&bytes[..queued.len], request(1, Request::Hello));
    for invalid in [
        request(2, Request::Hello),
        request(1, Request::Frame { crtc_id: 1 }),
        vec![0; protocol::REQUEST_LEN],
    ] {
        client.send(&invalid, &[]).unwrap();
        assert!(await_initial_hello(&authority, || Ok(())).is_err());
        authority.receive(&mut bytes).unwrap();
    }
    client.send(&request(1, Request::Hello), &[]).unwrap();
    assert!(await_initial_hello(&authority, || Err("revoked".into())).is_err());
}

#[test]
fn revocation_at_last_pre_spawn_check_retires_without_creating_a_helper() {
    let (client, authority) = SeqpacketChannel::pair().unwrap();
    let (receipt, retirement) = SeqpacketChannel::pair().unwrap();
    client.send(&request(1, Request::Hello), &[]).unwrap();
    let checks = AtomicUsize::new(0);
    let result = start_session(
        &authority,
        Some(&retirement),
        || {
            if checks.fetch_add(1, Ordering::SeqCst) == 3 {
                Err("revoked before spawn".into())
            } else {
                Ok(())
            }
        },
        || -> Result<(), String> { panic!("revocation must not allocate a native helper") },
    );
    assert_eq!(result.unwrap_err(), "revoked before spawn");
    let mut bytes = [0; 9];
    let response = receipt.receive(&mut bytes).unwrap();
    assert_eq!(&bytes[..response.len], AUTHORITY_SESSION_RETIRED);
}

#[test]
fn failed_and_lost_handoffs_retire_without_allocating_a_helper() {
    for malformed in [false, true] {
        let (client, authority) = SeqpacketChannel::pair().unwrap();
        let (receipt, retirement) = SeqpacketChannel::pair().unwrap();
        if malformed {
            client.send(b"bad HELLO", &[]).unwrap();
        } else {
            drop(client);
        }
        let result = start_session(
            &authority,
            Some(&retirement),
            || Ok(()),
            || -> Result<(), String> { panic!("no native allocation without valid HELLO") },
        );
        if malformed {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_none());
        }
        let mut bytes = [0; 9];
        let response = receipt.receive(&mut bytes).unwrap();
        assert_eq!(&bytes[..response.len], AUTHORITY_SESSION_RETIRED);
    }
}
