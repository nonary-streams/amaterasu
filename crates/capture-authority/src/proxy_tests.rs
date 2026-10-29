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
