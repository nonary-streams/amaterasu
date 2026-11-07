use super::*;

fn prepare_with_display(host: &mut Harness) -> u64 {
    let mut request = prepare();
    request["display_request"] = json!({
        "expected_topology_revision":"0123456789abcdef0123456789abcdef",
        "configuration":{"displays":[{"id":"first","width":1920,"height":1080,
            "refresh":{"numerator":60,"denominator":1},"x":0,"y":0}]},
        "mode_policies":[{"display_id":"first","remapping":{},"context":{
            "resolution_is_automatic":true,"refresh_rate_is_automatic":true,
            "client_display_mode_override":false,"optimize_game_settings":false}}]
    });
    let prepared = host.response(1, request);
    assert_eq!(prepared["state"], "prepared", "{prepared}");
    let id = prepared["session"].as_u64().unwrap();
    let mut request = start(id);
    request["configuration"]["capture_provider"] = json!("pipewire");
    request["configuration"]["output"] = json!(crate::pipeline::virtual_display::REQUESTED_OUTPUT);
    let started = host.response(2, request);
    assert_eq!(started["state"], "active", "{started}");
    id
}

fn removal_count(host: &Harness) -> usize {
    host.broker
        .calls
        .iter()
        .filter(|call| {
            call.service == "amaterasu.virtual-display/1" && call.payload["action"] == "remove"
        })
        .count()
}

#[test]
fn owned_output_survives_failed_restore_until_retry() {
    let mut host = Harness::new();
    let id = prepare_with_display(&mut host);
    let identity = crate::pipeline::virtual_display::identity(&host.pipeline.records[&id]);
    host.broker.display_restore_failures = 1;
    let failed = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(failed["state"], "cleanup_failed", "{failed}");
    assert!(host.broker.display_lease.is_some());
    assert!(host.broker.virtual_displays.contains_key(&identity));
    assert_eq!(removal_count(&host), 0);

    let stopped = host.response(4, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    assert!(host.broker.display_lease.is_none());
    assert!(!host.broker.virtual_displays.contains_key(&identity));
    assert_eq!(removal_count(&host), 1);
}

#[test]
fn owned_output_survives_failed_video_close_until_retry() {
    let mut host = Harness::new();
    let id = prepare_with_display(&mut host);
    let identity = crate::pipeline::virtual_display::identity(&host.pipeline.records[&id]);
    host.broker.fail_close = Some("amaterasu.video/1".into());
    let failed = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(failed["state"], "cleanup_failed", "{failed}");
    assert!(
        host.pipeline.records[&id]
            .resources
            .contains_key(&Resource::Video)
    );
    assert!(
        host.pipeline.records[&id]
            .attempted
            .contains(&Resource::Video)
    );
    assert!(
        !host.pipeline.records[&id]
            .resources
            .contains_key(&Resource::AudioCapture)
    );
    assert!(
        !host.pipeline.records[&id]
            .resources
            .contains_key(&Resource::AudioEncoder)
    );
    assert!(host.pipeline.records[&id].input_lease.is_none());
    assert!(host.broker.display_lease.is_some());
    assert!(host.broker.virtual_displays.contains_key(&identity));
    assert_eq!(removal_count(&host), 0);

    host.broker.fail_close = None;
    let stopped = host.response(4, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    assert!(!host.broker.virtual_displays.contains_key(&identity));
    assert_eq!(removal_count(&host), 1);
}

#[test]
fn owned_output_waits_for_delayed_video_retirement() {
    let mut host = Harness::new();
    let id = prepare_with_display(&mut host);
    let identity = crate::pipeline::virtual_display::identity(&host.pipeline.records[&id]);
    host.broker.video_close_polls = 6;
    host.broker
        .enqueue(3, "owner", json!({"action":"stop","session":id}));
    let mut observed_pending = false;
    let mut response = None;
    for _ in 0..600 {
        host.tick();
        if host.broker.video_remaining > 0 {
            observed_pending = true;
            assert!(host.broker.virtual_displays.contains_key(&identity));
            assert_eq!(removal_count(&host), 0);
            assert!(host.broker.display_lease.is_some());
        }
        if let Some(reply) = host.broker.replies.remove(&3) {
            response = Some(reply);
            break;
        }
    }
    assert!(observed_pending);
    assert_eq!(response.unwrap()["state"], "stopped");
    assert!(!host.broker.virtual_displays.contains_key(&identity));
    assert_eq!(removal_count(&host), 1);
}

#[test]
fn owned_output_survives_uncertain_video_close_until_reconciled() {
    let mut host = Harness::new();
    let id = prepare_with_display(&mut host);
    let identity = crate::pipeline::virtual_display::identity(&host.pipeline.records[&id]);
    host.broker.fault = Some(("amaterasu.video/1:close".into(), Fault::AfterMutation));
    host.broker.fail_cancel = true;
    let failed = host.response(3, json!({"action":"stop","session":id}));
    assert_eq!(failed["state"], "cleanup_failed", "{failed}");
    assert!(host.pipeline.records[&id].uncertain.is_some());
    assert!(host.broker.display_lease.is_some());
    assert!(host.broker.virtual_displays.contains_key(&identity));
    assert_eq!(removal_count(&host), 0);

    host.broker.fail_cancel = false;
    let stopped = host.response(4, json!({"action":"stop","session":id}));
    assert_eq!(stopped["state"], "stopped", "{stopped}");
    assert!(host.broker.display_lease.is_none());
    assert!(!host.broker.virtual_displays.contains_key(&identity));
    assert_eq!(removal_count(&host), 1);
}
