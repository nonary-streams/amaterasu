use super::*;
use crate::model::{Availability, PipeWireState, SourceDiagnostics, SourceStage};
use crate::test_support::{Driver, FakeBackend, settings, setup};
use std::sync::atomic::Ordering;

fn select(
    controller: &mut Controller<FakeBackend>,
    owner: &str,
    operation: &str,
    now: u64,
) -> CaptureResult<Reply> {
    controller.handle(
        owner,
        Operation::SelectSource {
            source_type: SourceType::Monitor,
            operation_id: operation.into(),
        },
        now,
    )
}
pub fn ready(controller: &mut Controller<FakeBackend>, driver: &Driver) -> String {
    select(controller, "administrator", "selection", 0).unwrap();
    driver.frame(100, 1000);
    *driver.state.lock().unwrap() = SourceState::Ready {
        node: 42,
        width: 2,
        height: 2,
    };
    controller.refresh(10);
    controller
        .handle(
            "amaterasu.video",
            Operation::Enumerate {
                offset: None,
                limit: None,
            },
            10,
        )
        .unwrap()
        .payload["outputs"][0]["id"]
        .as_str()
        .unwrap()
        .into()
}
pub fn open(
    controller: &mut Controller<FakeBackend>,
    output: &str,
    caller: &str,
    operation: &str,
) -> Reply {
    controller
        .handle(
            caller,
            Operation::Open {
                output: output.into(),
                operation_id: Some(operation.into()),
                constant_rate: None,
            },
            11,
        )
        .unwrap()
}

fn module(controller: Controller<FakeBackend>) -> crate::CaptureModule {
    use amaterasu_module_sdk::{RuntimeClient, abi::*, capture::SourcePreflightFactory};
    use std::{ffi::c_void, sync::Mutex, time::Instant};

    unsafe extern "C" fn unused_host(
        _context: *mut c_void,
        _request: BorrowedBytes,
        _reply: ReplySink,
    ) -> Status {
        STATUS_INTERNAL_ERROR
    }

    let controller = Arc::new(Mutex::new(
        controller.with_backend(crate::backend::NativeBackend::unavailable_for_test()),
    ));
    let factory = crate::native::Factory::new(controller.clone());
    // SAFETY: the retained factory remains live through the preflight wrapper;
    // this fixture opens no portal, PipeWire, or compositor connection.
    let preflight_factory = unsafe {
        SourcePreflightFactory::new(
            &factory.api,
            Arc::new(crate::PipewirePreflight(controller.clone())),
        )
    };
    // SAFETY: the stateless endpoint stays live for this fixture. Quiescence
    // cannot dispatch requests or touch native services through it.
    let runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::null_mut(),
            invoke: unused_host,
        })
    };
    crate::CaptureModule {
        runtime,
        controller,
        _factory: factory,
        _preflight_factory: preflight_factory,
        epoch: Instant::now(),
        pending_opens: Vec::new(),
        polled_request: None,
        shutting_down: false,
    }
}

#[test]
fn module_quiescence_releases_capture_before_reporting_ready() {
    use amaterasu_module_sdk::{ManagedModule, abi::STATUS_OK};

    let (mut controller, driver) = setup(settings());
    let output = ready(&mut controller, &driver);
    let reply = open(&mut controller, &output, "amaterasu.video", "one");
    let stream = reply.payload["stream"].as_u64().unwrap();
    let mut module = module(controller);

    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
    let reader = module
        .controller
        .lock()
        .unwrap()
        .reader("amaterasu.video", stream)
        .unwrap();
    let frame = reader.mailbox.claim().unwrap().unwrap();
    module.prepare_shutdown(20_000).unwrap();
    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
    assert!(!reader.admission.is_closed());
    let shared = module.controller.clone();
    let held = shared.lock().unwrap();
    assert_eq!(module.quiesce(), Ok(false));
    drop(held);
    assert_eq!(module.quiesce(), Ok(true));
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    assert!(reader.admission.is_closed());
    assert_eq!(frame.frame.bytes, vec![100; 16]);
    frame.finish(STATUS_OK);
    assert_eq!(module.quiesce(), Ok(true));
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
}

#[test]
fn close_waits_for_native_cleanup_and_inflight_callback() {
    let (mut controller, driver) = setup(settings());
    let output = ready(&mut controller, &driver);
    let stream = open(&mut controller, &output, "amaterasu.video", "one").payload["stream"]
        .as_u64()
        .unwrap();
    let admission = controller
        .reader("amaterasu.video", stream)
        .unwrap()
        .admission;
    let permit = admission.enter().unwrap();
    driver.retirement_ready.store(false, Ordering::SeqCst);
    assert_eq!(
        controller.close("amaterasu.video", stream),
        Err("source_retirement_pending")
    );
    assert!(admission.is_closed());
    assert_eq!(driver.retirement_started.load(Ordering::SeqCst), 1);
    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
    assert!(select(&mut controller, "administrator", "new", 20).is_err());
    driver.retirement_ready.store(true, Ordering::SeqCst);
    controller.refresh(21);
    assert_eq!(
        controller.close("amaterasu.video", stream),
        Err("source_retirement_pending")
    );
    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
    drop(permit);
    controller.refresh(22);
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    assert_eq!(controller.close("amaterasu.video", stream), Ok(()));
}

#[test]
fn cancelled_selection_keeps_retiring_source_until_cleanup_finishes() {
    let (mut controller, driver) = setup(settings());
    select(&mut controller, "administrator", "selection", 0).unwrap();
    driver.retirement_ready.store(false, Ordering::SeqCst);
    let cancel = || Operation::CancelSelection {
        operation_id: "selection".into(),
    };
    assert!(matches!(
        controller.handle("administrator", cancel(), 1),
        Err("source_retirement_pending")
    ));
    // Simulate a token callback that was already running when cancellation
    // first forgot the previous authorization.
    driver.restore_available.store(true, Ordering::SeqCst);
    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
    assert!(matches!(
        select(&mut controller, "administrator", "second", 2),
        Err("source_selection_busy")
    ));
    driver.retirement_ready.store(true, Ordering::SeqCst);
    controller.refresh(3);
    assert!(!driver.restore_available.load(Ordering::SeqCst));
    assert!(controller.handle("administrator", cancel(), 3).is_ok());
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
}

#[test]
fn named_retirement_consumes_capacity_and_waits_for_callback() {
    let mut config = settings();
    config.allow_named_outputs = true;
    let (mut controller, driver) = setup(config);
    *driver.state.lock().unwrap() = SourceState::Ready {
        node: 42,
        width: 2,
        height: 2,
    };
    let stream =
        open(&mut controller, "kwin:Virtual-1", "amaterasu.video", "one").payload["stream"]
            .as_u64()
            .unwrap();
    let permit = controller
        .reader("amaterasu.video", stream)
        .unwrap()
        .admission
        .enter()
        .unwrap();
    driver.retirement_ready.store(false, Ordering::SeqCst);
    assert_eq!(
        controller.close("amaterasu.video", stream),
        Err("source_retirement_pending")
    );
    for number in 2..=4 {
        open(
            &mut controller,
            &format!("kwin:Virtual-{number}"),
            "amaterasu.video",
            &format!("op-{number}"),
        );
    }
    assert!(matches!(
        controller.handle(
            "amaterasu.video",
            Operation::Open {
                output: "kwin:Virtual-5".into(),
                operation_id: Some("five".into()),
                constant_rate: None
            },
            20
        ),
        Err("resource_exhausted")
    ));
    driver.retirement_ready.store(true, Ordering::SeqCst);
    controller.refresh(21);
    assert_eq!(
        controller.close("amaterasu.video", stream),
        Err("source_retirement_pending")
    );
    drop(permit);
    controller.refresh(22);
    assert_eq!(controller.close("amaterasu.video", stream), Ok(()));
}

#[test]
fn module_shutdown_settles_pending_opens_and_rejects_new_sources() {
    use amaterasu_module_sdk::{ManagedModule, RuntimeClient, abi::*};
    use std::ffi::c_void;
    unsafe extern "C" fn shutdown_host(
        context: *mut c_void,
        request: BorrowedBytes,
        reply: ReplySink,
    ) -> Status {
        // SAFETY: this fixture retains the command bytes and exclusive response
        // vector for each synchronous runtime call.
        let Ok(bytes) = (unsafe { request.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice::<crate::Command>(bytes) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let response = match command {
            crate::Command::CheckRequest { .. } => crate::Response::Ok,
            crate::Command::Poll {} => crate::Response::Pending,
            crate::Command::Complete { request, payload } => {
                // SAFETY: the fixture's uniquely borrowed vector outlives the
                // runtime and is accessed only by this synchronous callback.
                unsafe { &mut *context.cast::<Vec<Value>>() }
                    .push(json!({"request":request,"payload":payload}));
                crate::Response::Ok
            }
            _ => return STATUS_INVALID_ARGUMENT,
        };
        let bytes = serde_json::to_vec(&response).unwrap();
        // SAFETY: the callback borrows this serialized response until it returns.
        unsafe { reply.send(&bytes) };
        STATUS_OK
    }

    let mut completed = Vec::<Value>::new();
    let (mut controller, _) = setup(settings());
    controller.shutdown();
    let mut module = module(controller);
    module.prepare_shutdown(20_000).unwrap();
    // SAFETY: the private response vector and stateless function remain live
    // until the module is explicitly destroyed below.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut completed as *mut Vec<Value>).cast(),
            invoke: shutdown_host,
        })
    };
    module.pending_opens.push(crate::PendingOpen {
        request: 17,
        caller: "amaterasu.video".into(),
        operation: Operation::Open {
            output: "portal:closed-source".into(),
            operation_id: Some("late-open".into()),
            constant_rate: None,
        },
    });
    assert_eq!(module.quiesce(), Ok(false));
    module.tick().unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["request"], 17);
    assert_eq!(completed[0]["payload"]["error"]["code"], "capture_shutdown");
    module
        .dispatch(
            18,
            "administrator".into(),
            Operation::SelectSource {
                source_type: SourceType::Monitor,
                operation_id: "late-selection".into(),
            },
            3,
        )
        .unwrap();
    assert_eq!(completed[1]["request"], 18);
    assert_eq!(completed[1]["payload"]["error"]["code"], "capture_shutdown");
    assert_eq!(module.quiesce(), Ok(true));
    drop(module);
}

#[test]
fn cancelled_open_retains_shared_selection_until_its_last_waiter_leaves() {
    use amaterasu_module_sdk::{ManagedModule, RuntimeClient, abi::*};
    use std::ffi::c_void;

    unsafe extern "C" fn host(_: *mut c_void, input: BorrowedBytes, reply: ReplySink) -> Status {
        // SAFETY: input and reply are borrowed for this synchronous call.
        let Ok(input) = (unsafe { input.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice::<crate::Command>(input) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let response = match command {
            crate::Command::CheckRequest { request: 1 } => {
                crate::Response::error("not_found", "cancelled")
            }
            crate::Command::CheckRequest { request: 2 } => crate::Response::Ok,
            crate::Command::Poll {} => crate::Response::Pending,
            _ => return STATUS_INVALID_ARGUMENT,
        };
        let bytes = serde_json::to_vec(&response).unwrap();
        // SAFETY: reply copies this response synchronously.
        unsafe { reply.send(&bytes) };
        STATUS_OK
    }
    let mut configuration = settings();
    configuration.allow_named_outputs = true;
    let (mut controller, driver) = setup(configuration);
    let operation = Operation::Open {
        output: "kwin:Virtual-2".into(),
        operation_id: Some("shared".into()),
        constant_rate: None,
    };
    assert!(matches!(
        controller.handle("amaterasu.video", operation.clone(), 0),
        Err("named_source_pending")
    ));
    let mut module = module(controller);
    // SAFETY: this stateless endpoint remains live throughout the test.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::null_mut(),
            invoke: host,
        })
    };
    for request in [1, 2] {
        module.pending_opens.push(crate::PendingOpen {
            request,
            caller: "amaterasu.video".into(),
            operation: operation.clone(),
        });
    }
    module.tick().unwrap();
    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
    assert_eq!(driver.selections.load(Ordering::SeqCst), 1);
    assert_eq!(module.pending_opens.len(), 1);
    let portal = Operation::Open {
        output: "portal-selected-session-source".into(),
        operation_id: Some("shared".into()),
        constant_rate: None,
    };
    module
        .cancel_unreferenced_open("amaterasu.video", &portal)
        .unwrap();
    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
    module.pending_opens[0].operation = portal;
    module
        .cancel_unreferenced_open("amaterasu.video", &operation)
        .unwrap();
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
}

#[test]
fn broker_failure_keeps_pending_open_identities_for_retry() {
    use amaterasu_module_sdk::ManagedModule;
    let mut configuration = settings();
    configuration.allow_named_outputs = true;
    let (mut controller, driver) = setup(configuration);
    let operation = Operation::Open {
        output: "kwin:Virtual-2".into(),
        operation_id: Some("shared".into()),
        constant_rate: None,
    };
    assert!(matches!(
        controller.handle("amaterasu.video", operation.clone(), 0),
        Err("named_source_pending")
    ));
    let mut module = module(controller);
    for request in [1, 2] {
        module.pending_opens.push(crate::PendingOpen {
            request,
            caller: "amaterasu.video".into(),
            operation: operation.clone(),
        });
    }
    // A native preflight may hold this lock. The host tick must not wait,
    // poll the failing endpoint, or dequeue either pending request.
    let shared = module.controller.clone();
    let guard = shared.lock().unwrap();
    assert!(module.tick().is_ok());
    assert_eq!(module.pending_opens.len(), 2);
    drop(guard);
    assert!(module.tick().is_err());
    assert_eq!(
        module
            .pending_opens
            .iter()
            .map(|pending| pending.request)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
}

#[test]
fn dequeued_request_survives_check_failure_and_is_retired_on_cancellation() {
    use amaterasu_module_sdk::{ManagedModule, RuntimeClient, abi::*};
    use std::ffi::c_void;
    #[derive(Default)]
    struct State {
        polls: usize,
        checks: usize,
    }
    unsafe extern "C" fn host(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> Status {
        // SAFETY: the fixture owns the state and input throughout this call.
        let state = unsafe { &mut *context.cast::<State>() };
        let Ok(input) = (unsafe { input.read() }) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice::<crate::Command>(input) else {
            return STATUS_INVALID_ARGUMENT;
        };
        let response = match command {
            crate::Command::Poll {} => {
                state.polls += 1;
                if state.polls == 1 {
                    crate::Response::Request {
                        id: 42,
                        caller: "amaterasu.video".into(),
                        initiator: "amaterasu.video".into(),
                        service: crate::SERVICE.into(),
                        payload: json!({"action":"status"}),
                        remaining_ms: 5000,
                    }
                } else {
                    crate::Response::Pending
                }
            }
            crate::Command::CheckRequest { request: 42 } => {
                state.checks += 1;
                if state.checks == 1 {
                    return STATUS_INTERNAL_ERROR;
                }
                crate::Response::error("request_ended", "cancelled")
            }
            _ => return STATUS_INVALID_ARGUMENT,
        };
        let bytes = serde_json::to_vec(&response).unwrap();
        // SAFETY: the reply copies bytes synchronously.
        unsafe { reply.send(&bytes) };
        STATUS_OK
    }
    let mut state = State::default();
    let (controller, _) = setup(settings());
    let mut module = module(controller);
    // SAFETY: fixture state and endpoint outlive the module.
    module.runtime = unsafe {
        RuntimeClient::from_endpoint(HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut state as *mut State).cast(),
            invoke: host,
        })
    };
    assert!(module.tick().is_err());
    assert_eq!(module.polled_request.as_ref().unwrap().id, 42);
    assert_eq!(state.polls, 1);
    module.tick().unwrap();
    assert!(module.polled_request.is_none());
    assert_eq!(state.checks, 2);
    assert_eq!(state.polls, 2);
    drop(module);
}

#[test]
fn named_cancellation_does_not_retire_a_waiting_portal_restore() {
    let mut configuration = settings();
    configuration.allow_named_outputs = true;
    configuration.remember_source = true;
    configuration
        .selection_principals
        .insert("amaterasu.video".into());
    let (mut controller, driver) = setup(configuration);
    driver.restore_available.store(true, Ordering::SeqCst);
    let portal = Operation::Open {
        output: "portal-selected-session-source".into(),
        operation_id: Some("portal".into()),
        constant_rate: None,
    };
    let named = Operation::Open {
        output: "kwin:Virtual-2".into(),
        operation_id: Some("named".into()),
        constant_rate: None,
    };
    assert!(matches!(
        controller.handle("amaterasu.video", portal.clone(), 0),
        Err("source_restore_pending")
    ));
    assert!(matches!(
        controller.handle("amaterasu.video", named.clone(), 0),
        Err("named_source_pending")
    ));
    let mut module = module(controller);
    module.pending_opens.push(crate::PendingOpen {
        request: 1,
        caller: "amaterasu.video".into(),
        operation: portal,
    });
    module
        .cancel_unreferenced_open("amaterasu.video", &named)
        .unwrap();
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    assert!(module.controller.lock().unwrap().selection.is_some());
}

#[test]
fn unsupported_constant_rate_does_not_allocate_or_replay_a_lease() {
    let (mut controller, driver) = setup(settings());
    let output = ready(&mut controller, &driver);
    let unsupported = || Operation::Open {
        output: output.clone(),
        operation_id: Some("video-1".into()),
        constant_rate: Some(true),
    };
    assert!(matches!(
        controller.handle("amaterasu.video", unsupported(), 11),
        Err("unsupported_constant_rate")
    ));
    assert!(controller.lease.is_none());
    assert_eq!(controller.opened, 0);
    open(&mut controller, &output, "amaterasu.video", "video-1");
    assert!(matches!(
        controller.handle("amaterasu.video", unsupported(), 12),
        Err("unsupported_constant_rate")
    ));
    assert_eq!(controller.opened, 1);
}

#[test]
fn disabled_or_unallowlisted_selection_and_open_never_touch_the_driver() {
    let (mut controller, driver) = setup(Settings::default());
    assert!(matches!(
        select(&mut controller, "administrator", "selection", 0),
        Err("capture_disabled")
    ));
    assert!(matches!(
        controller.handle(
            "amaterasu.video",
            Operation::Open {
                output: "guessed".into(),
                operation_id: None,
                constant_rate: None
            },
            0
        ),
        Err("capture_disabled")
    ));
    controller.settings.enabled = true;
    controller
        .settings
        .capture_principals
        .insert("amaterasu.video".into());
    assert!(matches!(
        select(&mut controller, "administrator", "selection", 0),
        Err("selection_permission_denied")
    ));
    assert!(matches!(
        controller.handle(
            "amaterasu.video",
            Operation::Open {
                output: "guessed".into(),
                operation_id: None,
                constant_rate: None
            },
            0
        ),
        Err("source_selection_required")
    ));
    let status = controller
        .handle("administrator", Operation::Status, 0)
        .unwrap()
        .payload;
    assert_eq!(status["selection_state"], "permission_required");
    assert_eq!(status["availability"]["source_types"], 3);
    assert_eq!(driver.selections.load(Ordering::SeqCst), 0);
}

#[test]
fn enumeration_is_empty_until_actual_ready_and_geometry_is_not_invented() {
    let (mut controller, driver) = setup(settings());
    let initial = controller
        .handle(
            "amaterasu.video",
            Operation::Enumerate {
                offset: None,
                limit: None,
            },
            0,
        )
        .unwrap()
        .payload;
    assert_eq!(initial["outputs"], json!([]));
    assert_eq!(initial["selection_choices"], json!(["monitor", "window"]));
    let output = ready(&mut controller, &driver);
    let page = controller
        .handle(
            "amaterasu.video",
            Operation::Enumerate {
                offset: None,
                limit: Some(1),
            },
            15,
        )
        .unwrap()
        .payload;
    assert_eq!(page["outputs"][0]["id"], output);
    assert_eq!(page["outputs"][0]["width"], 2);
    assert!(page["outputs"][0]["desktop_geometry"].is_null());
    assert_eq!(page["outputs"][0]["geometry_supported"], false);
}

#[test]
fn capture_principals_are_explicit_and_status_does_not_leak_selection_operation_ids() {
    let (mut controller, driver) = setup(settings());
    let output = ready(&mut controller, &driver);

    let denied_enumeration = controller.handle(
        "other",
        Operation::Enumerate {
            offset: None,
            limit: None,
        },
        11,
    );
    assert!(matches!(
        denied_enumeration,
        Err("capture_permission_denied")
    ));
    let denied_open = controller.handle(
        "other",
        Operation::Open {
            output: output.clone(),
            operation_id: Some("other-open".into()),
            constant_rate: None,
        },
        11,
    );
    assert!(matches!(denied_open, Err("capture_permission_denied")));
    assert!(controller.lease.is_none());

    let selection_owner = controller
        .handle("administrator", Operation::Status, 11)
        .unwrap()
        .payload;
    assert_eq!(selection_owner["selection"]["operation_id"], "selection");
    let capture_principal = controller
        .handle("amaterasu.video", Operation::Status, 11)
        .unwrap()
        .payload;
    assert!(capture_principal["selection"]["operation_id"].is_null());
    assert_eq!(capture_principal["selection"]["output"]["id"], output);
    let unrelated = controller
        .handle("other", Operation::Status, 11)
        .unwrap()
        .payload;
    assert!(unrelated["selection"].is_null());
}

#[test]
fn selection_operations_are_owner_scoped_replayed_without_picker_and_cancelled_once() {
    let (mut controller, driver) = setup(settings());
    let first = select(&mut controller, "administrator", "selection", 0).unwrap();
    assert!(matches!(first.created, Some(Created::Selection)));
    assert!(
        select(&mut controller, "administrator", "selection", 1)
            .unwrap()
            .created
            .is_none()
    );
    assert!(matches!(
        controller.handle(
            "administrator",
            Operation::SelectSource {
                source_type: SourceType::Window,
                operation_id: "selection".into()
            },
            1
        ),
        Err("operation_conflict")
    ));
    assert!(matches!(
        controller.handle(
            "other",
            Operation::PollSelection {
                operation_id: "selection".into()
            },
            1
        ),
        Err("selection_not_found")
    ));
    assert!(matches!(
        controller.handle(
            "other",
            Operation::CancelSelection {
                operation_id: "selection".into()
            },
            1
        ),
        Err("selection_not_found")
    ));
    controller
        .handle(
            "administrator",
            Operation::CancelSelection {
                operation_id: "selection".into(),
            },
            2,
        )
        .unwrap();
    controller
        .handle(
            "administrator",
            Operation::CancelSelection {
                operation_id: "selection".into(),
            },
            3,
        )
        .unwrap();
    assert_eq!(driver.selections.load(Ordering::SeqCst), 1);
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    assert!(
        select(&mut controller, "administrator", "selection", 4)
            .unwrap()
            .created
            .is_none()
    );
}

#[test]
fn owned_lease_open_lookup_retry_close_and_failed_completion_cleanup() {
    let (mut controller, driver) = setup(settings());
    let output = ready(&mut controller, &driver);
    let first = open(&mut controller, &output, "amaterasu.video", "open");
    let id = first.payload["stream"].as_u64().unwrap();
    assert_eq!(
        open(&mut controller, &output, "amaterasu.video", "open").payload,
        first.payload
    );
    assert!(
        open(&mut controller, &output, "amaterasu.video", "open")
            .created
            .is_none()
    );
    assert!(controller.reader("other", id).is_err());
    assert!(matches!(
        controller.handle(
            "other",
            Operation::Lookup {
                operation_id: "open".into()
            },
            12
        ),
        Err("not_found")
    ));
    assert!(matches!(
        controller.handle("other", Operation::Close { stream: id }, 12),
        Err("not_found")
    ));
    controller.rollback("amaterasu.video", None);
    assert!(controller.reader("amaterasu.video", id).is_ok());
    controller.rollback("amaterasu.video", first.created);
    controller.close("amaterasu.video", id).unwrap();
    assert!(controller.close("other", id).is_err());
    assert!(controller.reader("amaterasu.video", id).is_err());
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        controller
            .handle("administrator", Operation::Status, 20)
            .unwrap()
            .payload["selection_state"],
        "permission_required"
    );
}

#[test]
fn idle_selection_expires_without_status_extending_its_deadline_and_shutdown_cancels_pending() {
    let (mut controller, driver) = setup(settings());
    ready(&mut controller, &driver);
    controller
        .handle("administrator", Operation::Status, 2009)
        .unwrap();
    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
    controller.refresh(2010);
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    select(&mut controller, "administrator", "new-selection", 2020).unwrap();
    controller.shutdown();
    controller.shutdown();
    assert_eq!(driver.drops.load(Ordering::SeqCst), 2);
    assert!(controller.selection.is_none());
}

#[test]
fn deny_revoke_and_timeout_release_sessions_and_active_leases() {
    for state in [
        SourceState::Denied,
        SourceState::Failed("native_failure"),
        SourceState::Revoked,
    ] {
        let (mut controller, driver) = setup(settings());
        let output = ready(&mut controller, &driver);
        let id = open(&mut controller, &output, "amaterasu.video", "open").payload["stream"]
            .as_u64()
            .unwrap();
        *driver.state.lock().unwrap() = state;
        controller.refresh(50);
        assert!(controller.reader("amaterasu.video", id).is_err());
        assert!(controller.selection.is_none());
        assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    }
    let (mut controller, driver) = setup(settings());
    select(&mut controller, "administrator", "selection", 0).unwrap();
    controller.refresh(5000);
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    let result = controller
        .handle(
            "administrator",
            Operation::PollSelection {
                operation_id: "selection".into(),
            },
            5001,
        )
        .unwrap()
        .payload;
    assert_eq!(result["error"], "source_selection_timeout");
}

#[test]
fn timeout_status_distinguishes_waiting_for_start_from_pipewire_connection_failure() {
    let (mut controller, driver) = setup(settings());
    select(&mut controller, "administrator", "start-timeout", 0).unwrap();
    *driver.diagnostics.lock().unwrap() = SourceDiagnostics {
        stage: Some(SourceStage::Start),
        ..SourceDiagnostics::default()
    };
    controller.refresh(5000);
    let timed_out = controller
        .handle(
            "administrator",
            Operation::PollSelection {
                operation_id: "start-timeout".into(),
            },
            5001,
        )
        .unwrap()
        .payload;
    assert_eq!(timed_out["error"], "source_selection_timeout");
    assert_eq!(timed_out["diagnostics"]["stage"], "start");
    assert!(timed_out["diagnostics"].get("pipewire_node").is_none());
    assert!(timed_out["diagnostics"].get("pipewire_serial").is_none());
    assert!(timed_out["diagnostics"].get("pipewire_state").is_none());

    let (mut controller, driver) = setup(settings());
    select(&mut controller, "administrator", "pipewire-error", 0).unwrap();
    *driver.diagnostics.lock().unwrap() = SourceDiagnostics {
        stage: Some(SourceStage::ConnectPipeWire),
        pipewire_node: Some(93),
        pipewire_serial: Some(98),
        pipewire_state: Some(PipeWireState::Error),
        pipewire_error: Some(-2),
        adapter_error_code: Some(4),
    };
    *driver.state.lock().unwrap() = SourceState::Failed("pipewire_failed");
    controller.refresh(10);
    let failed = controller
        .handle(
            "administrator",
            Operation::PollSelection {
                operation_id: "pipewire-error".into(),
            },
            11,
        )
        .unwrap()
        .payload;
    assert_eq!(failed["diagnostics"]["stage"], "connect_pipewire");
    assert_eq!(failed["diagnostics"]["pipewire_node"], 93);
    assert_eq!(failed["diagnostics"]["pipewire_serial"], 98);
    assert_eq!(failed["diagnostics"]["pipewire_state"], "error");
    assert_eq!(failed["diagnostics"]["pipewire_error"], -2);
    assert_eq!(failed["diagnostics"]["adapter_error_code"], 4);
    assert!(failed["diagnostics"].get("session_path").is_none());
    assert!(failed["diagnostics"].get("fd").is_none());
    assert!(failed["diagnostics"].get("token").is_none());
}

#[test]
fn settings_and_selection_bounds_fail_before_native_allocation() {
    assert!(Settings::default().validate().is_ok());
    let mut policy = settings();
    policy.idle_timeout_seconds = 600;
    assert!(policy.validate().is_ok());
    policy.idle_timeout_seconds = 601;
    assert_eq!(policy.validate(), Err("invalid_settings"));
    policy.idle_timeout_seconds = 60;
    policy.selection_timeout_seconds = 301;
    assert_eq!(policy.validate(), Err("invalid_settings"));
    let (mut controller, driver) = setup(settings());
    controller.backend.availability = Availability::unsupported("system_dependencies_unavailable");
    assert!(matches!(
        select(&mut controller, "administrator", "selection", 0),
        Err("system_dependencies_unavailable")
    ));
    assert_eq!(driver.selections.load(Ordering::SeqCst), 0);
    assert!(serde_json::from_value::<Operation>(json!({"action":"select_source","source_type":"monitor","operation_id":"one","caller":"administrator"})).is_err());
    for index in 0..MAX_SELECTIONS {
        controller.backend.availability.available = true;
        controller.backend.availability.source_types = 3;
        let operation = format!("selection-{index}");
        select(&mut controller, "administrator", &operation, 0).unwrap();
        controller
            .handle(
                "administrator",
                Operation::CancelSelection {
                    operation_id: operation,
                },
                0,
            )
            .unwrap();
    }
    assert!(matches!(
        select(&mut controller, "administrator", "too-many", 0),
        Err("resource_exhausted")
    ));
}

#[test]
fn remembered_selection_restores_only_for_allowed_callers_and_waits_for_real_frames() {
    let mut policy = settings();
    policy.remember_source = true;
    policy.selection_principals.insert("amaterasu.video".into());
    let (mut controller, driver) = setup(policy);
    driver.restore_available.store(true, Ordering::SeqCst);
    let request = || Operation::Open {
        output: "portal-selected-session-source".into(),
        operation_id: Some("resume".into()),
        constant_rate: None,
    };
    assert!(matches!(
        controller.handle("stranger", request(), 0),
        Err("capture_permission_denied")
    ));
    assert_eq!(driver.selections.load(Ordering::SeqCst), 0);
    assert!(matches!(
        controller.handle("amaterasu.video", request(), 0),
        Err("source_restore_pending")
    ));
    assert!(controller.lease.is_none());
    assert_eq!(driver.selections.load(Ordering::SeqCst), 1);
    // A repeated pending open must not launch another picker or consume another token.
    assert!(matches!(
        controller.handle("amaterasu.video", request(), 1),
        Err("source_restore_pending")
    ));
    assert_eq!(driver.selections.load(Ordering::SeqCst), 1);
    driver.frame(50, 1000);
    *driver.state.lock().unwrap() = SourceState::Ready {
        node: 42,
        width: 2,
        height: 2,
    };
    let opened = controller.handle("amaterasu.video", request(), 10).unwrap();
    assert!(opened.payload["ok"].as_bool().unwrap());
    let id = opened.payload["stream"].as_u64().unwrap();
    controller.close("amaterasu.video", id).unwrap();
    assert!(controller.lease.is_none());
    assert!(controller.selection.is_none());
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    // Reconnects reuse an internal record instead of exhausting selection history.
    for _ in 0..35 {
        assert!(matches!(
            controller.handle("amaterasu.video", request(), 20),
            Err("source_restore_pending")
        ));
        let opened = controller.handle("amaterasu.video", request(), 21).unwrap();
        controller
            .close(
                "amaterasu.video",
                opened.payload["stream"].as_u64().unwrap(),
            )
            .unwrap();
    }
    assert_eq!(controller.selections.len(), 1);
}

#[test]
fn restoration_is_opt_in_and_cancelled_opens_do_not_leave_capture_running() {
    let (mut controller, driver) = setup(settings());
    driver.restore_available.store(true, Ordering::SeqCst);
    let request = || Operation::Open {
        output: "portal-selected-session-source".into(),
        operation_id: Some("resume".into()),
        constant_rate: None,
    };
    assert!(matches!(
        controller.handle("amaterasu.video", request(), 0),
        Err("source_selection_required")
    ));
    assert_eq!(driver.selections.load(Ordering::SeqCst), 0);
    controller.settings.remember_source = true;
    assert!(matches!(
        controller.handle("amaterasu.video", request(), 0),
        Err("source_selection_required")
    ));
    controller
        .settings
        .selection_principals
        .insert("amaterasu.video".into());
    assert!(matches!(
        controller.handle("amaterasu.video", request(), 0),
        Err("source_restore_pending")
    ));
    controller.cancel_pending_restore("stranger");
    assert_eq!(driver.drops.load(Ordering::SeqCst), 0);
    controller.cancel_pending_restore("amaterasu.video");
    assert_eq!(driver.drops.load(Ordering::SeqCst), 1);
    assert!(controller.selection.is_none());
}

#[test]
fn named_outputs_have_independent_capture_leases_and_cancel_does_not_close_a_peer() {
    let mut settings = crate::test_support::settings();
    settings.allow_named_outputs = true;
    let (mut controller, driver) = crate::test_support::setup(settings);
    let open = |output: &str, operation: &str| Operation::Open {
        output: output.into(),
        operation_id: Some(operation.into()),
        constant_rate: None,
    };
    assert!(matches!(
        controller.handle("amaterasu.video", open("kwin:Virtual-2", "first"), 0),
        Err("named_source_pending")
    ));
    *driver.state.lock().unwrap() = SourceState::Ready {
        node: 1,
        width: 2,
        height: 2,
    };
    driver.frame(42, 1);
    let first = controller
        .handle("amaterasu.video", open("kwin:Virtual-2", "first"), 1)
        .unwrap()
        .payload["stream"]
        .as_u64()
        .unwrap();
    let second = controller
        .handle("amaterasu.video", open("kwin:Virtual-3", "second"), 1)
        .unwrap()
        .payload["stream"]
        .as_u64()
        .unwrap();
    assert_ne!(first, second);
    controller.close("amaterasu.video", first).unwrap();
    assert!(controller.reader("amaterasu.video", first).is_err());
    assert!(controller.reader("amaterasu.video", second).is_ok());
    assert!(controller.reader("other.module", second).is_err());
    controller.cancel_pending_restore("amaterasu.video");
    assert!(controller.reader("amaterasu.video", second).is_ok());
    controller.shutdown();
    assert!(controller.reader("amaterasu.video", second).is_err());
}

#[test]
fn named_capture_reports_only_observed_live_output() {
    let mut settings = crate::test_support::settings();
    settings.allow_named_outputs = true;
    let (mut controller, driver) = crate::test_support::setup(settings);
    let open = || Operation::Open {
        output: "kwin:Virtual-12".into(),
        operation_id: Some("test-observation".into()),
        constant_rate: None,
    };
    *driver.state.lock().unwrap() = SourceState::Ready {
        node: 1,
        width: 2,
        height: 2,
    };
    let first = controller
        .handle("amaterasu.video", open(), 0)
        .unwrap()
        .payload;
    assert!(first.get("observed_capture").is_none());
    *driver.observed.lock().unwrap() = Some(crate::backend::ObservedCapture {
        version: 1,
        protocol: "wayland-wl-output",
        compositor_id: "a".repeat(64),
        registry_global: 73,
        output_name: "Virtual-12".into(),
        width: 1920,
        height: 1080,
        refresh_millihz: 60000,
    });
    let second = controller
        .handle("amaterasu.video", open(), 1)
        .unwrap()
        .payload;
    assert_eq!(second["observed_capture"]["registry_global"], 73);
    assert_eq!(second["observed_capture"]["width"], 1920);
    driver.observed.lock().unwrap().as_mut().unwrap().width = 2560;
    let refreshed = controller
        .direct
        .lookup("amaterasu.video", "test-observation")
        .unwrap();
    assert_eq!(refreshed["observed_capture"]["width"], 2560);
    *driver.state.lock().unwrap() = SourceState::Revoked;
    let lookup = controller
        .direct
        .lookup("amaterasu.video", "test-observation")
        .unwrap();
    assert!(lookup.get("observed_capture").is_none());
}
