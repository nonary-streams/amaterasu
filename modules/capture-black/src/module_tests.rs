use super::*;
use crate::model::Operation;
use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use std::collections::{BTreeSet, VecDeque};
use std::ffi::c_void;

#[derive(Default)]
struct Broker {
    requests: VecDeque<Response>,
    active: BTreeSet<u64>,
    completed: Vec<(u64, serde_json::Value)>,
}

impl Broker {
    fn close(&mut self, request: u64, caller: &str, stream: u64) {
        self.active.insert(request);
        self.requests.push_back(Response::Request {
            id: request,
            caller: caller.into(),
            initiator: caller.into(),
            service: SERVICE.into(),
            payload: serde_json::json!({"action":"close","stream":stream}),
            remaining_ms: 30_000,
        });
    }
}

unsafe extern "C" fn broker_runtime(
    context: *mut c_void,
    input: BorrowedBytes,
    sink: ReplySink,
) -> Status {
    // SAFETY: each test retains its boxed broker until after the module drops.
    let broker = unsafe { &*context.cast::<Mutex<Broker>>() };
    // SAFETY: RuntimeClient retains the command bytes for this invocation.
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut broker = broker.lock().unwrap();
    let response = match command {
        Command::Poll {} => broker.requests.pop_front().unwrap_or(Response::Pending),
        Command::CheckRequest { request } if broker.active.contains(&request) => Response::Ok,
        Command::CheckRequest { .. } => Response::Error {
            code: "request_ended".into(),
            message: "fixture request was cancelled".into(),
        },
        Command::Complete { request, payload } if broker.active.remove(&request) => {
            broker.completed.push((request, payload));
            Response::Ok
        }
        Command::Complete { .. } => Response::Error {
            code: "request_ended".into(),
            message: "fixture request was cancelled".into(),
        },
        _ => panic!("unexpected black capture broker command"),
    };
    // SAFETY: RuntimeClient provides a live reply sink for this call.
    unsafe { sink.send(&serde_json::to_vec(&response).unwrap()) };
    STATUS_OK
}

fn module_with_broker(
    controller: native::SharedController,
    broker: &mut Box<Mutex<Broker>>,
) -> BlackCapture {
    let factory = native::Factory::new(controller.clone());
    // SAFETY: both boxed factories remain owned by the module under test.
    let preflight_factory = unsafe {
        SourcePreflightFactory::new(&factory.api, Arc::new(BlackPreflight(controller.clone())))
    };
    // SAFETY: the boxed broker remains stable and live through module ticks.
    let runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::from_mut(broker.as_mut()).cast(),
            invoke: broker_runtime,
        })
    };
    BlackCapture {
        runtime,
        controller,
        pending_closes: Vec::new(),
        _factory: factory,
        _preflight_factory: preflight_factory,
    }
}

fn active_delivery() -> (native::SharedController, u64, crate::controller::Delivery) {
    let mut controller = Controller::new(true);
    let (reply, _) = controller
        .handle(
            "video",
            Operation::Open {
                output: "black:64x48@60:nv12".into(),
                operation_id: Some("active-close".into()),
                resource_capture_clock: None,
                resource_callback_contract: None,
            },
            1_000_000,
            &mut || Ok(()),
        )
        .unwrap();
    let id = reply["stream"].as_u64().unwrap();
    let delivery = controller.prepare("video", id, 1_000_000, false).unwrap();
    (Arc::new(Mutex::new(controller)), id, delivery)
}

#[test]
fn close_request_stays_pending_until_the_active_delivery_releases_its_lease() {
    let (controller, stream, delivery) = active_delivery();
    let pixels = Arc::downgrade(&delivery.pixels);
    let mut broker = Box::<Mutex<Broker>>::default();
    broker.lock().unwrap().close(41, "video", stream);
    let mut module = module_with_broker(controller.clone(), &mut broker);

    module.tick().unwrap();
    assert!(broker.lock().unwrap().completed.is_empty());
    assert_eq!(module.pending_closes.len(), 1);
    assert!(pixels.upgrade().is_some());
    assert!(matches!(
        controller
            .lock()
            .unwrap()
            .prepare("video", stream, 1_000_002, false),
        Err(amaterasu_media::MEDIA_CLOSED)
    ));

    assert_eq!(delivery.finish(STATUS_OK), STATUS_OK);
    module.tick().unwrap();

    let broker_state = broker.lock().unwrap();
    assert_eq!(broker_state.completed.len(), 1);
    assert_eq!(broker_state.completed[0].0, 41);
    assert_eq!(broker_state.completed[0].1, serde_json::json!({"ok":true}));
    drop(broker_state);
    let mut controller = controller.lock().unwrap();
    assert!(controller.mode("video", stream).is_err());
    assert_eq!(controller.close("video", stream, 1_000_005), Ok(()));
    assert!(pixels.upgrade().is_none());
}

#[test]
fn cancelling_a_pending_close_releases_only_the_waiter_and_still_retires() {
    let (controller, stream, delivery) = active_delivery();
    let pixels = Arc::downgrade(&delivery.pixels);
    let mut broker = Box::<Mutex<Broker>>::default();
    broker.lock().unwrap().close(42, "video", stream);
    let mut module = module_with_broker(controller.clone(), &mut broker);

    module.tick().unwrap();
    broker.lock().unwrap().active.remove(&42);
    module.tick().unwrap();
    assert_eq!(module.pending_closes.len(), 1);
    assert!(broker.lock().unwrap().completed.is_empty());

    assert_eq!(delivery.finish(STATUS_OK), STATUS_OK);
    module.tick().unwrap();

    assert!(module.pending_closes.is_empty());
    assert!(broker.lock().unwrap().completed.is_empty());
    let mut controller = controller.lock().unwrap();
    assert!(controller.mode("video", stream).is_err());
    assert_eq!(controller.close("video", stream, 1_000_005), Ok(()));
    assert!(pixels.upgrade().is_none());
}
