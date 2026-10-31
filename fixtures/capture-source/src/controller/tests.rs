use super::*;

#[test]
fn open_clock_agreement_is_validated_owned_and_idempotent() {
    let mut controller = Controller::default();
    let request = |epoch: &str| {
        json!({"action":"open", "output":OUTPUT,
        "operation_id":"clock-test", "resource_callback_contract":"cross_service_safe", "resource_capture_clock": {
            "domain":"host_monotonic", "epoch_id":epoch}})
    };
    let parse = |value| serde_json::from_value::<Operation>(value).unwrap();
    let token = "0123456789abcdef0123456789abcdef";
    let (reply, created) = controller.handle("owner", parse(request(token))).unwrap();
    assert!(created.is_some());
    assert_eq!(reply["resource_callback_contract"], "cross_service_safe");
    assert_eq!(
        reply["resource_capture_clock"],
        request(token)["resource_capture_clock"]
    );
    let (retry, created) = controller.handle("owner", parse(request(token))).unwrap();
    assert_eq!(retry, reply);
    assert_eq!(created, None);
    assert_eq!(
        controller.handle("owner", parse(request("11111111111111111111111111111111"))),
        Err("operation_conflict")
    );
    let (other, _) = controller
        .handle("other", parse(request("11111111111111111111111111111111")))
        .unwrap();
    assert_ne!(other["stream"], reply["stream"]);
    let lookup = controller
        .handle(
            "owner",
            Operation::Lookup {
                operation_id: "clock-test".into(),
            },
        )
        .unwrap()
        .0;
    assert_eq!(
        lookup["resource_capture_clock"],
        reply["resource_capture_clock"]
    );
    for invalid in [
        "",
        "00000000000000000000000000000000",
        "ABCDEF0123456789abcdef0123456789",
        "gggggggggggggggggggggggggggggggg",
    ] {
        assert_eq!(
            controller.handle("bad", parse(request(invalid))),
            Err("invalid_resource_capture_clock")
        );
    }
    let mut wrong_domain = request(token);
    wrong_domain["resource_capture_clock"]["domain"] = json!("stream_relative");
    assert_eq!(
        controller.handle("bad", parse(wrong_domain)),
        Err("invalid_resource_capture_clock")
    );
    let mut null_clock = request(token);
    null_clock["resource_capture_clock"] = Value::Null;
    assert!(serde_json::from_value::<Operation>(null_clock).is_err());
    for contract in [None, Some("unsupported")] {
        let mut value = request(token);
        if let Some(contract) = contract {
            value["resource_callback_contract"] = json!(contract);
        } else {
            value
                .as_object_mut()
                .unwrap()
                .remove("resource_callback_contract");
        }
        assert_eq!(
            controller.handle("bad", parse(value)),
            Err("invalid_resource_callback_contract")
        );
    }
    let mut null_contract = request(token);
    null_contract["resource_callback_contract"] = Value::Null;
    assert!(serde_json::from_value::<Operation>(null_contract).is_err());
}

fn open(
    controller: &mut Controller,
    caller: &str,
    operation: Option<&str>,
) -> (Value, Option<u64>) {
    controller
        .handle(
            caller,
            Operation::Open {
                output: OUTPUT.into(),
                operation_id: operation.map(str::to_owned),
                resource_capture_clock: None,
                resource_callback_contract: None,
            },
        )
        .unwrap()
}

#[test]
fn open_retry_and_lookup_are_scoped_to_the_authenticated_caller() {
    let mut controller = Controller::default();
    let (first, created) = open(&mut controller, "amaterasu.video", Some("launch-capture"));
    assert_eq!(created, first["stream"].as_u64());
    assert_eq!(
        open(&mut controller, "amaterasu.video", Some("launch-capture")),
        (first.clone(), None)
    );
    let (foreign, _) = open(&mut controller, "other", Some("launch-capture"));
    assert_ne!(first["stream"], foreign["stream"]);
    for (caller, expected) in [("amaterasu.video", first), ("other", foreign)] {
        assert_eq!(
            controller
                .handle(
                    caller,
                    Operation::Lookup {
                        operation_id: "launch-capture".into(),
                    }
                )
                .unwrap()
                .0,
            expected
        );
    }
    assert_eq!(controller.metrics()["opened"], 2);
    assert_eq!(
        controller.handle(
            "amaterasu.video",
            Operation::Open {
                output: "different-output".into(),
                operation_id: Some("launch-capture".into()),
                resource_capture_clock: None,
                resource_callback_contract: None,
            }
        ),
        Err("operation_conflict")
    );
}

#[test]
fn exact_id_close_is_idempotent_only_for_its_owner_and_shutdown_counts_cleanup() {
    let mut controller = Controller::default();
    let (_, created) = open(&mut controller, "owner", Some("operation"));
    let id = created.unwrap();
    assert_eq!(controller.close("other", id), Err("not_found"));
    controller.close("owner", id).unwrap();
    controller.close("owner", id).unwrap();
    assert_eq!(controller.close("other", id), Err("not_found"));
    assert!(matches!(
        controller.prepare_frame("owner", id),
        Err(MEDIA_CLOSED)
    ));
    assert_eq!(controller.lookup("owner", "operation"), Err("not_found"));
    let (_, replacement) = open(&mut controller, "owner", Some("operation"));
    assert_ne!(replacement, Some(id));
    open(&mut controller, "other", None);
    controller.shutdown();
    controller.shutdown();
    let metrics = controller.metrics();
    assert_eq!(metrics["opened"], 3);
    assert_eq!(metrics["closed"], 3);
    assert_eq!(metrics["active"], 0);
    assert_eq!(metrics["callers"]["owner"]["closed"], 2);
}

#[test]
fn completion_rollback_removes_only_the_newly_created_stream() {
    let mut controller = Controller::default();
    let (_, original) = open(&mut controller, "owner", Some("retained"));
    let (_, retry) = open(&mut controller, "owner", Some("retained"));
    controller.rollback_open("owner", retry);
    assert!(controller.authorize("owner", original.unwrap()).is_ok());
    let (_, created) = open(&mut controller, "owner", Some("cancelled"));
    controller.rollback_open("owner", created);
    assert!(matches!(
        controller.prepare_frame("owner", created.unwrap()),
        Err(MEDIA_CLOSED)
    ));
    assert_eq!(controller.lookup("owner", "cancelled"), Err("not_found"));
    assert_eq!(controller.metrics()["closed"], 1);
    assert_eq!(controller.metrics()["active"], 1);
}

#[test]
fn rejected_and_concurrent_deliveries_preserve_sequence_and_finite_frame_budget() {
    let mut controller = Controller::default();
    let id = open(&mut controller, "owner", None).1.unwrap();
    let rejected = controller.prepare_frame("owner", id).unwrap();
    let pixels = rejected.bytes.clone();
    assert!(matches!(
        controller.prepare_frame("owner", id),
        Err(MEDIA_BUSY)
    ));
    rejected.finish(MEDIA_BUSY);
    assert_eq!(controller.metrics()["frames"], 0);
    let retry = controller.prepare_frame("owner", id).unwrap();
    assert_eq!(retry.sequence, 0);
    assert_eq!(retry.bytes, pixels);
    retry.finish(STATUS_OK);
    for expected in 1..FRAME_COUNT {
        let frame = controller.prepare_frame("owner", id).unwrap();
        assert_eq!(frame.sequence, expected);
        frame.finish(STATUS_OK);
    }
    for _ in 0..3 {
        assert!(matches!(
            controller.prepare_frame("owner", id),
            Err(MEDIA_PENDING)
        ));
    }
    assert_eq!(controller.metrics()["frames"], FRAME_COUNT);
    assert_eq!(
        controller.metrics()["callers"]["owner"]["frames"],
        FRAME_COUNT
    );
}

#[test]
fn output_geometry_enumeration_and_input_validation_are_explicit() {
    let mut controller = Controller::default();
    let page = controller
        .handle(
            "owner",
            Operation::Enumerate {
                offset: None,
                limit: Some(1),
            },
        )
        .unwrap()
        .0;
    assert_eq!(page["outputs"][0]["id"], OUTPUT);
    assert_eq!(page["outputs"][0]["width"], WIDTH);
    assert_eq!(page["outputs"][0]["height"], HEIGHT);
    let metadata = open(&mut controller, "owner", None).0;
    let actual: amaterasu_media::CaptureGeometry =
        serde_json::from_value(metadata["desktop_geometry"].clone()).unwrap();
    assert_eq!(actual, geometry());
    assert_eq!(actual.validate(), Ok(()));
    assert_eq!(
        controller.handle("other", Operation::Status).unwrap().0["streams"],
        json!([])
    );
    for (offset, limit) in [(Some(1), None), (None, Some(0)), (None, Some(257))] {
        assert_eq!(
            controller.handle("owner", Operation::Enumerate { offset, limit }),
            Err("invalid_request")
        );
    }
    for operation in ["".into(), "invalid\noperation".into(), "x".repeat(129)] {
        assert_eq!(
            controller.handle(
                "owner",
                Operation::Lookup {
                    operation_id: operation
                }
            ),
            Err("invalid_operation_id")
        );
    }
    assert!(
        serde_json::from_value::<Operation>(
            json!({"action":"open", "output":OUTPUT, "caller":"owner"})
        )
        .is_err()
    );
}

#[test]
fn active_and_lifetime_open_limits_bound_retained_fixture_state() {
    let mut controller = Controller::default();
    for _ in 0..MAX_STREAMS {
        open(&mut controller, "owner", None);
    }
    let attempt = || Operation::Open {
        output: OUTPUT.into(),
        operation_id: None,
        resource_capture_clock: None,
        resource_callback_contract: None,
    };
    assert_eq!(
        controller.handle("owner", attempt()),
        Err("resource_exhausted")
    );
    controller.shutdown();
    for _ in MAX_STREAMS as u64..MAX_OPENS {
        let id = open(&mut controller, "owner", None).1.unwrap();
        controller.close("owner", id).unwrap();
    }
    assert_eq!(
        controller.handle("new-owner", attempt()),
        Err("resource_exhausted")
    );
    assert_eq!(controller.metrics()["opened"], MAX_OPENS);
    assert_eq!(controller.metrics()["closed"], MAX_OPENS);
    assert_eq!(controller.closed.len(), MAX_OPENS as usize);
}
