//! Exercise the exported provider through its worker, without native displays.
use super::*;
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

struct Endpoint {
    pending: AtomicBool,
    cancelled: AtomicBool,
    entered: SyncSender<()>,
    release: Mutex<Receiver<()>>,
    completed: Mutex<Vec<Value>>,
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: the test retains its Arc until the worker joins, and all mutable
    // fixture state uses atomics or a mutex. The SDK owns input for this call.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let response = match command {
        Command::GetSettings {} => Response::Result {
            payload: json!({"enabled":false}),
        },
        Command::Register { service } => {
            assert_eq!(service, SERVICE);
            Response::Ok
        }
        Command::Poll {} if endpoint.pending.swap(false, Ordering::AcqRel) => Response::Request {
            id: 1,
            caller: "stream-owner".into(),
            initiator: "stream-owner".into(),
            service: SERVICE.into(),
            payload: json!({"action":"status"}),
            remaining_ms: 1000,
        },
        Command::Poll {} => Response::Pending,
        Command::CheckRequest { .. } => {
            endpoint.entered.send(()).unwrap();
            endpoint.release.lock().unwrap().recv().unwrap();
            if endpoint.cancelled.load(Ordering::Acquire) {
                Response::error("request_ended", "cancelled")
            } else {
                Response::Ok
            }
        }
        Command::Complete { payload, .. } => {
            endpoint.completed.lock().unwrap().push(payload);
            Response::Ok
        }
        Command::NotifyWorkReady {} => Response::Ok,
        other => panic!("unexpected provider command: {other:?}"),
    };
    // SAFETY: the SDK copies these owned response bytes before this returns.
    unsafe { reply.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

#[test]
fn blocked_virtual_provider_keeps_host_ticks_responsive_and_joins_after_completion() {
    for cancelled in [false, true] {
        let (entered, arrival) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let endpoint = Arc::new(Endpoint {
            pending: AtomicBool::new(true),
            cancelled: AtomicBool::new(false),
            entered,
            release: Mutex::new(gate),
            completed: Mutex::new(Vec::new()),
        });
        let api = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: Arc::as_ptr(&endpoint).cast_mut().cast(),
            invoke,
        };
        // SAFETY: endpoint remains owned until this service and its worker retire.
        let runtime = unsafe { RuntimeClient::from_endpoint(api) };
        let mut provider = VirtualDisplayService::start(runtime).unwrap();
        let (continue_host, gate_host) = mpsc::sync_channel(1);
        let (returned, result) = mpsc::sync_channel(1);
        let lifetime = endpoint.clone();
        let host = std::thread::spawn(move || {
            let _lifetime = lifetime;
            provider.tick().unwrap();
            gate_host.recv().unwrap();
            for _ in 0..1000 {
                provider.tick().unwrap();
            }
            provider.prepare_shutdown(2000).unwrap();
            let ready = provider.quiesce().unwrap();
            assert!(returned.send((provider, ready)).is_ok());
        });
        let arrived = arrival.recv_timeout(Duration::from_secs(3));
        continue_host.send(()).unwrap();
        let responsive = result.recv_timeout(Duration::from_secs(3));
        // Release even when a responsiveness assertion would fail, so teardown
        // cannot strand the worker or retain a borrowed host endpoint.
        endpoint.cancelled.store(cancelled, Ordering::Release);
        release.send(()).unwrap();
        arrived.expect("provider did not reach the admitted request");
        let (mut provider, ready) = responsive.expect("host tick waited on provider work");
        host.join().unwrap();
        assert!(
            !ready,
            "provider retired before its accepted request completed"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while !provider.quiesce().unwrap() {
            assert!(Instant::now() < deadline, "provider worker did not retire");
            std::thread::yield_now();
        }
        let completed = endpoint.completed.lock().unwrap();
        assert_eq!(completed.len(), 1);
        if cancelled {
            assert_eq!(completed[0]["code"], "request_ended");
        } else {
            assert_eq!(completed[0]["ok"], true);
        }
    }
}
