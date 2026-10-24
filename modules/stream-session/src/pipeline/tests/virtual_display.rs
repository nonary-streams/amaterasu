use super::*;
fn own_display_start(session: u64) -> Value {
    let mut request = start(session);
    request["configuration"]["capture_provider"] = json!("pipewire");
    request["configuration"]["output"] = json!(crate::pipeline::virtual_display::REQUESTED_OUTPUT);
    request
}
#[test]
fn display_is_owned_before_capture_and_removed_only_after_video_closes() {
    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, own_display_start(session))["state"],
        "active"
    );
    let identity = crate::pipeline::virtual_display::identity(&host.pipeline.records[&session]);
    assert!(host.broker.virtual_displays.contains_key(&identity));
    let create = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "create"
        })
        .unwrap();
    let capture = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "open"
        })
        .unwrap();
    assert!(create < capture);
    assert_eq!(
        host.broker.calls[capture].payload["output"],
        "kwin:Virtual-2"
    );
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "stopped"
    );
    assert!(host.broker.virtual_displays.is_empty());
    let close = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == Resource::Video.service() && call.payload["action"] == "close"
        })
        .unwrap();
    let remove = host
        .broker
        .calls
        .iter()
        .position(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "remove"
        })
        .unwrap();
    assert!(close < remove);
}
#[test]
fn failed_connector_cleanup_retains_the_write_ahead_record() {
    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, own_display_start(session))["state"],
        "active"
    );
    host.broker.virtual_display_fail_remove = true;
    assert_eq!(
        host.response(3, json!({"action":"stop","session":session}))["state"],
        "cleanup_failed"
    );
    assert!(host.pipeline.records[&session].virtual_display_attempted);
    assert!(
        host.broker.state_value.as_ref().unwrap()["entries"][session.to_string()]["virtual_display_attempted"]
            == true
    );
}
#[test]
fn restart_recovers_the_exact_monitor_identity_without_new_allocation() {
    let mut host = Harness::new();
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, own_display_start(session))["state"],
        "active"
    );
    let identity = crate::pipeline::virtual_display::identity(&host.pipeline.records[&session]);
    let document = host.broker.state_value.clone().unwrap();
    let mut restarted = Harness::with_restart_state(document, host.broker.state_revision);
    restarted.broker.virtual_displays = host.broker.virtual_displays.clone();
    restarted
        .broker
        .seed_session(&format!("ss-{session:x}-session"));
    // Other resources were not recreated; lookup confirms their absence.
    restarted.idle();
    assert!(!restarted.broker.virtual_displays.contains_key(&identity));
    assert!(
        !restarted
            .broker
            .calls
            .iter()
            .any(|call| call.service == "amaterasu.virtual-display/1"
                && call.payload["action"] == "create")
    );
}

#[test]
fn owned_output_centers_the_pointer_and_refreshes_absolute_mapping_when_a_peer_arrives() {
    use amaterasu_media::{CaptureGeometry, DesktopRect};
    let mut host = Harness::new();
    host.pipeline.settings.enable_input = true;
    let first = CaptureGeometry::new(
        DesktopRect::new(1536, 0, 1920, 1080).unwrap(),
        DesktopRect::new(0, 0, 3456, 1080).unwrap(),
    )
    .unwrap();
    host.broker.desktop_geometry = Some(json!(first));
    host.broker.input_absolute_pointer = Some(json!(true));
    host.plane.borrow_mut().desktop_geometry = Some(first);
    let session = host.response(1, prepare())["session"].as_u64().unwrap();
    assert_eq!(
        host.response(2, own_display_start(session))["state"],
        "active"
    );
    host.idle();
    let bounds = first.normalized_output().unwrap();
    assert!(
        host.broker
            .input_events
            .iter()
            .any(|event| event["kind"] == "absolute"
                && event["x"] == (u32::from(bounds.left) + u32::from(bounds.right)) / 2)
    );
    let expanded =
        CaptureGeometry::new(first.output, DesktopRect::new(0, 0, 5376, 1080).unwrap()).unwrap();
    host.plane.borrow_mut().desktop_geometry = Some(expanded);
    host.idle();
    assert_eq!(
        host.pipeline.records[&session].capture_geometry,
        Some(expanded)
    );
}
