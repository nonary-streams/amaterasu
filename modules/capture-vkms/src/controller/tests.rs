use super::*;
use crate::test_support::{FakeBackend, HELPER, Scripted, connector, settings};

const VIDEO: &str = "amaterasu.video";

fn controller() -> (Controller<FakeBackend>, FakeBackend) {
    let backend = FakeBackend::new();
    let handles = backend.handles();
    (Controller::new(backend, settings()), handles)
}

fn open(output: &str, operation: &str) -> Operation {
    Operation::Open {
        output: output.into(),
        operation_id: Some(operation.into()),
        constant_rate: None,
    }
}

fn opened(controller: &mut Controller<FakeBackend>) -> u64 {
    controller
        .handle(VIDEO, open("kwin:Virtual-9", "video-1"), 0)
        .unwrap()
        .created
        .unwrap()
}

#[test]
fn open_resolves_the_crtc_and_reports_whole_desktop_geometry() {
    let (mut controller, backend) = controller();
    let reply = controller
        .handle(VIDEO, open("kwin:Virtual-9", "video-1"), 0)
        .unwrap();
    let id = reply.created.unwrap();
    let payload = reply.payload;
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["stream"], id);
    assert_eq!(payload["output"], "kwin:Virtual-9");
    assert_eq!(payload["crtc_id"], 41);
    assert_eq!(payload["geometry_supported"], true);
    let geometry: CaptureGeometry = serde_json::from_value::<amaterasu_media::DesktopRect>(
        payload["desktop_geometry"]["output"].clone(),
    )
    .map(|output| CaptureGeometry::new(output, output).unwrap())
    .unwrap();
    assert_eq!(controller.reader(VIDEO, id).unwrap().geometry, geometry);
    assert_eq!(
        *backend.connects.lock().unwrap(),
        vec![(HELPER.to_owned(), 41)]
    );
}

#[test]
fn open_replays_lookup_and_close_are_caller_scoped() {
    let (mut controller, backend) = controller();
    let id = opened(&mut controller);
    let replay = controller
        .handle(VIDEO, open("kwin:Virtual-9", "video-1"), 1)
        .unwrap();
    assert_eq!(replay.created, None);
    assert_eq!(replay.payload["stream"], id);
    assert_eq!(backend.connects.lock().unwrap().len(), 1);
    assert_eq!(
        controller
            .handle(VIDEO, open("kwin:Virtual-10", "video-1"), 1)
            .err()
            .unwrap()
            .code,
        "operation_conflict"
    );
    let lookup = controller
        .handle(
            VIDEO,
            Operation::Lookup {
                operation_id: "video-1".into(),
            },
            1,
        )
        .unwrap();
    assert_eq!(lookup.payload["stream"], id);

    assert_eq!(
        controller.close("other.module", id).unwrap_err().code,
        "not_found"
    );
    let frames = controller.reader(VIDEO, id).unwrap().frames;
    controller.close(VIDEO, id).unwrap();
    assert!(frames.admission.enter().is_none());
    controller.close(VIDEO, id).unwrap();
    assert_eq!(
        controller.reader(VIDEO, id).err().unwrap().code,
        "not_found"
    );
}

#[test]
fn definite_refusals_are_error_replies_with_codes() {
    let (mut controller, backend) = controller();
    let code = |controller: &mut Controller<FakeBackend>, operation| {
        controller.handle(VIDEO, operation, 0).err().unwrap().code
    };
    assert_eq!(
        code(&mut controller, open("kwin:Virtual-0", "a")),
        "invalid_named_output"
    );
    assert_eq!(
        code(
            &mut controller,
            Operation::Open {
                output: "kwin:Virtual-9".into(),
                operation_id: None,
                constant_rate: None,
            }
        ),
        "operation_id_required"
    );
    assert_eq!(
        code(
            &mut controller,
            Operation::Open {
                output: "kwin:Virtual-9".into(),
                operation_id: Some("a".into()),
                constant_rate: Some(true),
            }
        ),
        "unsupported_constant_rate"
    );
    assert_eq!(
        code(&mut controller, open("kwin:Virtual-3", "a")),
        "output_not_found"
    );
    *backend.helper.lock().unwrap() = Err(CaptureError::permission_denied(HELPER));
    assert_eq!(
        code(&mut controller, open("kwin:Virtual-9", "a")),
        "permission_denied"
    );
    *backend.connectors.lock().unwrap() = Err(CaptureError::driver_unavailable("none"));
    assert_eq!(
        code(&mut controller, open("kwin:Virtual-9", "a")),
        "driver_unavailable"
    );
    let payload = CaptureError::code("output_not_found").payload();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "output_not_found");
}

#[test]
fn a_missing_helper_setting_is_frame_helper_unavailable() {
    let mut without_helper = settings();
    without_helper.frame_helper = None;
    let mut controller = Controller::new(FakeBackend::new(), without_helper);
    let error = controller
        .handle(VIDEO, open("kwin:Virtual-9", "a"), 0)
        .err()
        .unwrap();
    assert_eq!(error.code, "frame_helper_unavailable");

    let enumeration = controller
        .handle(
            VIDEO,
            Operation::Enumerate {
                offset: None,
                limit: None,
            },
            0,
        )
        .unwrap()
        .payload;
    assert_eq!(enumeration["availability"]["available"], false);
    assert_eq!(
        enumeration["availability"]["reason"],
        "frame_helper_unavailable"
    );
    assert_eq!(enumeration["checks"]["driver"]["ok"], true);
    assert_eq!(enumeration["outputs"].as_array().unwrap().len(), 2);
}

#[test]
fn policy_gates_open_and_enumerate() {
    let mut disabled = settings();
    disabled.enabled = false;
    let mut controller = Controller::new(FakeBackend::new(), disabled);
    assert_eq!(
        controller
            .handle(VIDEO, open("kwin:Virtual-9", "a"), 0)
            .err()
            .unwrap()
            .code,
        "capture_disabled"
    );
    let (mut controller, _) = self::controller();
    let enumerate = Operation::Enumerate {
        offset: None,
        limit: None,
    };
    assert_eq!(
        controller
            .handle("stranger", enumerate.clone(), 0)
            .err()
            .unwrap()
            .code,
        "capture_permission_denied"
    );
    assert_eq!(
        controller
            .handle(
                VIDEO,
                Operation::Enumerate {
                    offset: Some(1),
                    limit: None
                },
                0
            )
            .err()
            .unwrap()
            .code,
        "invalid_request"
    );
}

#[test]
fn enumeration_reports_permission_and_driver_reasons() {
    let (mut controller, backend) = controller();
    let enumerate = Operation::Enumerate {
        offset: None,
        limit: None,
    };
    let ready = controller
        .handle(VIDEO, enumerate.clone(), 0)
        .unwrap()
        .payload;
    assert_eq!(ready["availability"]["available"], true);
    assert_eq!(ready["outputs"][0]["id"], "kwin:Virtual-9");
    assert_eq!(ready["outputs"][0]["attached"], true);

    *backend.helper.lock().unwrap() = Err(CaptureError::permission_denied(HELPER));
    let denied = controller
        .handle(VIDEO, enumerate.clone(), 0)
        .unwrap()
        .payload;
    assert_eq!(denied["availability"]["reason"], "permission_denied");
    assert!(
        denied["availability"]["message"]
            .as_str()
            .unwrap()
            .contains("setcap cap_sys_admin+p")
    );

    *backend.helper.lock().unwrap() = Ok(());
    *backend.connectors.lock().unwrap() = Err(CaptureError::driver_unavailable("gone"));
    let missing = controller.handle(VIDEO, enumerate, 0).unwrap().payload;
    assert_eq!(missing["availability"]["reason"], "driver_unavailable");
    assert!(missing["outputs"].as_array().unwrap().is_empty());

    let status = controller
        .handle(VIDEO, Operation::Status, 0)
        .unwrap()
        .payload;
    assert_eq!(status["availability"]["reason"], "driver_unavailable");
}

#[test]
fn refresh_follows_a_crtc_change_and_a_mode_change() {
    let (mut controller, backend) = controller();
    let id = opened(&mut controller);
    *backend.connectors.lock().unwrap() = Ok(vec![connector(9, 55, 2560)]);
    controller.refresh(RESOLVE_INTERVAL_MS - 1);
    assert_eq!(controller.reader(VIDEO, id).unwrap().frames.crtc_id(), 41);
    controller.refresh(RESOLVE_INTERVAL_MS);
    let reader = controller.reader(VIDEO, id).unwrap();
    assert_eq!(reader.frames.crtc_id(), 55);
    assert_eq!(reader.geometry.output.width, 2560);
    assert_eq!(reader.frames.fault(), None);
}

#[test]
fn a_crtc_lookup_failure_triggers_an_immediate_refresh() {
    let (mut controller, backend) = controller();
    let id = opened(&mut controller);
    backend.script.push(Scripted::Refuse(
        amaterasu_vkms_frame_helper::protocol::Failure::CrtcNotFound,
    ));
    let frames = controller.reader(VIDEO, id).unwrap().frames;
    assert!(frames.lend().is_err());
    *backend.connectors.lock().unwrap() = Ok(vec![connector(9, 60, 1920)]);
    controller.refresh(1);
    assert_eq!(frames.crtc_id(), 60);
}

#[test]
fn a_disconnected_or_replaced_output_faults_the_stream() {
    let (mut controller, backend) = controller();
    let id = opened(&mut controller);
    let mut gone = connector(9, 41, 1920);
    gone.connection = crate::discovery::Connection::Disconnected;
    *backend.connectors.lock().unwrap() = Ok(vec![gone]);
    controller.refresh(RESOLVE_INTERVAL_MS);
    let frames = controller.reader(VIDEO, id).unwrap().frames;
    assert_eq!(
        frames.fault().map(StreamFault::code),
        Some("output_disconnected")
    );
    let status = controller
        .handle(VIDEO, Operation::Status, 0)
        .unwrap()
        .payload;
    assert_eq!(status["streams"][0]["fault"], "output_disconnected");

    let (mut controller, backend) = self::controller();
    let id = opened(&mut controller);
    let mut replaced = connector(9, 41, 1920);
    replaced.connector_id = 999;
    *backend.connectors.lock().unwrap() = Ok(vec![replaced]);
    controller.refresh(RESOLVE_INTERVAL_MS);
    assert_eq!(
        controller
            .reader(VIDEO, id)
            .unwrap()
            .frames
            .fault()
            .map(StreamFault::code),
        Some("output_replaced")
    );
}

#[test]
fn stream_limits_and_rollback() {
    let (mut controller, _) = controller();
    for index in 0..MAX_STREAMS {
        controller
            .handle(VIDEO, open("kwin:Virtual-9", &format!("op-{index}")), 0)
            .unwrap();
    }
    assert_eq!(
        controller
            .handle(VIDEO, open("kwin:Virtual-9", "one-more"), 0)
            .err()
            .unwrap()
            .code,
        "resource_exhausted"
    );
    let (mut controller, _) = self::controller();
    let reply = controller
        .handle(VIDEO, open("kwin:Virtual-9", "lost"), 0)
        .unwrap();
    controller.rollback(VIDEO, reply.created);
    let status = controller
        .handle(VIDEO, Operation::Status, 0)
        .unwrap()
        .payload;
    assert_eq!(status["active"], 0);
    assert_eq!(status["closed"], 1);
}

#[test]
fn shutdown_closes_admission_for_every_stream() {
    let (mut controller, _) = controller();
    let id = opened(&mut controller);
    let frames = controller.reader(VIDEO, id).unwrap().frames;
    controller.shutdown();
    assert!(frames.admission.enter().is_none());
    assert_eq!(
        controller
            .handle(VIDEO, open("kwin:Virtual-9", "after"), 0)
            .err()
            .unwrap()
            .code,
        "capture_disabled"
    );
}
