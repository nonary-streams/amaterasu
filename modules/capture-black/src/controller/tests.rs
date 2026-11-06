use super::*;
use crate::model::failure;

fn open(controller: &mut Controller, caller: &str, operation_id: &str, now: u64) -> u64 {
    controller
        .handle(
            caller,
            Operation::Open {
                output: "black:64x48@60:nv12".into(),
                operation_id: Some(operation_id.into()),
                resource_capture_clock: None,
                resource_callback_contract: None,
            },
            now,
            &mut || Ok(()),
        )
        .unwrap()
        .0["stream"]
        .as_u64()
        .unwrap()
}

#[test]
fn generated_nv12_black_has_valid_geometry_levels_and_canonical_bounded_output() {
    let mode = SourceMode::parse("black:1920x1080@120:nv12").unwrap();
    let pixels = mode.pixels().unwrap();
    let luma = 1920 * 1080;
    assert_eq!(pixels.len(), luma * 3 / 2);
    assert!(pixels[..luma].iter().all(|byte| *byte == 16));
    assert!(pixels[luma..].iter().all(|byte| *byte == 128));
    assert_eq!(mode.geometry().output, mode.geometry().virtual_desktop);
    assert_eq!(mode.output(), "black:1920x1080@120:nv12");
    for bad in [
        "desktop",
        "black:64x48@0:nv12",
        "black:63x48@60:nv12",
        "black:8194x48@60:nv12",
        "black:064x48@60:nv12",
        "black:64x48@241:nv12",
        "black:64x48@60:p010",
        "black:64x48@+60:nv12",
        "black:64x48@60:nv12:extra",
        "black:64x48@60.0:nv12",
    ] {
        assert!(SourceMode::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn admission_and_lookup_are_owner_scoped_and_retries_preserve_stream_identity() {
    let mut controller = Controller::new(true);
    let id = open(&mut controller, "first", "operation", 1_000_000);
    assert_eq!(open(&mut controller, "first", "operation", 1_000_001), id);
    let foreign = open(&mut controller, "other", "operation", 1_000_002);
    assert_ne!(id, foreign);
    assert!(controller.mode("other", id).is_err());
    assert_eq!(controller.close("other", id, 1_000_003), Err("not_found"));
    assert_eq!(controller.select("first", id, id + 1, 1), MEDIA_UNSUPPORTED);
    assert_eq!(controller.select("first", id, id, 2), MEDIA_UNSUPPORTED);
    assert_eq!(controller.select("first", id, id, 1), STATUS_OK);
    let conflict = controller.handle(
        "first",
        Operation::Open {
            output: "black:128x48@60:nv12".into(),
            operation_id: Some("operation".into()),
            resource_capture_clock: None,
            resource_callback_contract: None,
        },
        1_000_004,
        &mut || Ok(()),
    );
    assert_eq!(conflict.unwrap_err(), "operation_conflict");
    controller.close("first", id, 1_000_005).unwrap();
    controller.close("first", id, 1_000_006).unwrap();
    assert_eq!(
        controller
            .handle(
                "first",
                Operation::Open {
                    output: "black:64x48@60:nv12".into(),
                    operation_id: Some("operation".into()),
                    resource_capture_clock: None,
                    resource_callback_contract: None
                },
                1_000_007,
                &mut || Ok(())
            )
            .unwrap_err(),
        "operation_closed"
    );
    assert!(controller.mode("other", foreign).is_ok());
}

#[test]
fn rejected_delivery_retries_exact_identity_and_cadence_does_not_accumulate_a_queue() {
    let mut controller = Controller::new(true);
    let id = open(&mut controller, "owner", "paced", 1_000_000);
    let first = controller.prepare("owner", id, 1_000_000, false).unwrap();
    let identity = first.identity;
    assert_eq!(identity.sequence, 1);
    assert_eq!(first.finish(MEDIA_BUSY), MEDIA_BUSY);
    let retry = controller.prepare("owner", id, 2_000_000, false).unwrap();
    assert_eq!(retry.identity.sequence, identity.sequence);
    assert_eq!(retry.identity.timestamp_us, identity.timestamp_us);
    retry.finish(STATUS_OK);
    let current = controller.prepare("owner", id, 2_000_000, false).unwrap();
    assert_eq!(current.identity.sequence, 61);
    assert_eq!(current.identity.timestamp_us, 2_000_000);
    current.finish(STATUS_OK);
    assert!(matches!(
        controller.prepare("owner", id, 2_000_001, false),
        Err(MEDIA_PENDING)
    ));
    let next = controller.prepare("owner", id, 2_016_666, false).unwrap();
    assert_eq!(next.identity.sequence, 62);
    next.finish(STATUS_OK);
}

#[test]
fn close_and_lost_open_completion_wait_for_inflight_callbacks_then_retire_storage() {
    let mut controller = Controller::new(true);
    let id = open(&mut controller, "owner", "lost", 1_000_000);
    let frame = controller.prepare("owner", id, 1_000_000, false).unwrap();
    assert_eq!(controller.close("owner", id, 1_000_001), Err("busy"));
    assert!(matches!(
        controller.prepare("owner", id, 1_000_002, false),
        Err(MEDIA_CLOSED)
    ));
    controller.rollback_open("owner", Some(id), 1_000_003);
    assert!(controller.bytes > 0);
    drop(frame);
    controller.maintain(1_000_004);
    assert_eq!(controller.bytes, 0);
    assert!(controller.streams.is_empty());
    assert!(controller.retired.contains_key(&id));
    let next = open(&mut controller, "owner", "next", 1_000_005);
    assert!(next > id);
}

#[test]
fn cancelled_open_rechecks_authority_after_allocating_and_leaves_no_stream() {
    let mut controller = Controller::new(true);
    let mut calls = 0;
    let result = controller.handle(
        "owner",
        Operation::Open {
            output: "black:64x48@60:nv12".into(),
            operation_id: Some("cancelled".into()),
            resource_capture_clock: None,
            resource_callback_contract: None,
        },
        1_000_000,
        &mut || {
            calls += 1;
            if calls == 1 {
                Ok(())
            } else {
                Err("request_ended")
            }
        },
    );
    assert_eq!(result.unwrap_err(), "request_ended");
    assert_eq!(controller.next_id, 0);
    assert_eq!(controller.bytes, 0);
    assert!(controller.streams.is_empty());
    assert_eq!(failure("request_ended")["ok"], false);
}

#[test]
fn stream_and_memory_limits_fail_before_allocation_and_closed_ledger_is_bounded() {
    let mut controller = Controller::new(true);
    for index in 0..MAX_STREAMS {
        open(
            &mut controller,
            "owner",
            &format!("stream-{index}"),
            1_000_000,
        );
    }
    let result = controller.handle(
        "owner",
        Operation::Open {
            output: "black:64x48@60:nv12".into(),
            operation_id: Some("overflow".into()),
            resource_capture_clock: None,
            resource_callback_contract: None,
        },
        1_000_000,
        &mut || Ok(()),
    );
    assert_eq!(result.unwrap_err(), "resource_exhausted");
    assert_eq!(controller.streams.len(), MAX_STREAMS);
    controller.shutdown();
    assert_eq!(controller.bytes, 0);
    let request = || Operation::Open {
        output: "black:8192x8192@60:nv12".into(),
        operation_id: None,
        resource_capture_clock: None,
        resource_callback_contract: None,
    };
    controller
        .handle("owner", request(), 2_000_000, &mut || Ok(()))
        .unwrap();
    controller
        .handle("owner", request(), 2_000_000, &mut || Ok(()))
        .unwrap();
    assert_eq!(
        controller
            .handle("owner", request(), 2_000_000, &mut || Ok(()))
            .unwrap_err(),
        "resource_exhausted"
    );
    controller.shutdown();
    for index in 0..MAX_LEDGER {
        controller.retired.insert(
            index as u64,
            Retired {
                owner: "owner".into(),
                operation_id: None,
                retired_at_us: 2_000_000,
            },
        );
    }
    assert_eq!(
        controller
            .handle("owner", request(), 2_000_001, &mut || Ok(()))
            .unwrap_err(),
        "resource_exhausted"
    );
    controller.maintain(RETIRED_OPERATION_US + 2_000_001);
    assert!(controller.retired.is_empty());
}

#[test]
fn resource_clock_contract_is_explicit_and_conflicting_retry_is_rejected() {
    let mut controller = Controller::new(true);
    let request = |epoch: &str, contract: Option<&str>| Operation::Open {
        output: "black:64x48@60:nv12".into(),
        operation_id: Some("clock".into()),
        resource_capture_clock: Some(ResourceCaptureClock {
            domain: "host_monotonic".into(),
            epoch_id: epoch.into(),
        }),
        resource_callback_contract: contract.map(str::to_owned),
    };
    assert_eq!(
        controller
            .handle(
                "owner",
                request("11111111111111111111111111111111", None),
                1_000_000,
                &mut || Ok(())
            )
            .unwrap_err(),
        "invalid_resource_callback_contract"
    );
    assert_eq!(
        controller
            .handle(
                "owner",
                request(
                    "00000000000000000000000000000000",
                    Some("cross_service_safe")
                ),
                1_000_000,
                &mut || Ok(())
            )
            .unwrap_err(),
        "invalid_resource_capture_clock"
    );
    let (reply, created) = controller
        .handle(
            "owner",
            request(
                "11111111111111111111111111111111",
                Some("cross_service_safe"),
            ),
            1_000_000,
            &mut || Ok(()),
        )
        .unwrap();
    assert_eq!(reply["resource_callback_contract"], "cross_service_safe");
    let id = created.unwrap();
    let frame = controller.prepare("owner", id, 1_000_000, false).unwrap();
    assert_eq!(frame.clock_epoch, Some([0x11; 16]));
    drop(frame);
    assert_eq!(
        controller
            .handle(
                "owner",
                request(
                    "22222222222222222222222222222222",
                    Some("cross_service_safe")
                ),
                1_000_000,
                &mut || Ok(())
            )
            .unwrap_err(),
        "operation_conflict"
    );
}
