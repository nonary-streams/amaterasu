use super::*;
use crate::model::Operation;
use serde_json::{Value, json};

fn operation(value: Value) -> Operation {
    serde_json::from_value(value).unwrap()
}

fn error_code(value: &Value) -> Option<&str> {
    value["error"]["code"].as_str()
}

fn acquire(
    controller: &mut Controller,
    owner: &str,
    operation_id: Option<&str>,
    ttl_ms: u64,
    now: Instant,
) -> Value {
    let mut payload = json!({"action": "acquire", "ttl_ms": ttl_ms});
    if let Some(operation_id) = operation_id {
        payload["operation_id"] = operation_id.into();
    }
    controller.handle(owner, operation(payload), now)
}

fn lease(value: &Value) -> &str {
    value["lease"].as_str().expect("active lease token")
}

#[test]
fn disabled_default_never_allocates_or_records_input() {
    let now = Instant::now();
    let mut controller = Controller::new(false);
    let rejected = acquire(&mut controller, "owner", Some("disabled"), 1000, now);
    assert_eq!(error_code(&rejected), Some("injection_disabled"));
    assert_eq!(
        controller.metrics(now),
        json!({
            "ok": true, "fixture": true, "enabled": false, "active": false,
            "acquired": 0, "renewed": 0, "released": 0, "expired": 0,
            "events": [], "events_truncated": false,
            "held_keys": 0, "held_buttons": 0, "requested_policy": null,
        })
    );
}

#[test]
fn acquire_retry_is_owner_scoped_and_does_not_extend_the_lease() {
    let start = Instant::now();
    let mut controller = Controller::new(true);
    let opened = acquire(&mut controller, "owner", Some("session.input"), 1000, start);
    let token = lease(&opened).to_owned();
    assert_eq!(opened["state"], "active");
    assert_eq!(opened["backend"], BACKEND);
    assert_eq!(opened["cleanup_required"], false);
    assert_eq!(opened["absolute_pointer"], true);

    let retry = acquire(
        &mut controller,
        "owner",
        Some("session.input"),
        1000,
        start + Duration::from_millis(400),
    );
    assert_eq!(lease(&retry), token);
    assert_eq!(retry["expires_in_ms"], 600);
    assert_eq!(controller.metrics(start)["acquired"], 1);

    let conflict = acquire(&mut controller, "owner", Some("session.input"), 2000, start);
    assert_eq!(error_code(&conflict), Some("operation_conflict"));
    let foreign = controller.handle(
        "other",
        operation(json!({"action":"lookup","operation_id":"session.input"})),
        start,
    );
    assert_eq!(error_code(&foreign), Some("not_found"));
    let busy = acquire(&mut controller, "other", Some("other.input"), 1000, start);
    assert_eq!(error_code(&busy), Some("input_busy"));

    controller.cancel_owner("other");
    assert!(controller.metrics(start)["active"].as_bool().unwrap());
    controller.cancel_owner("owner");
    let tombstone = controller.handle(
        "owner",
        operation(json!({"action":"lookup","operation_id":"session.input"})),
        start,
    );
    assert_eq!(tombstone["state"], "released");
    assert_eq!(tombstone["reason"], "cancelled");
    assert!(tombstone["lease"].is_null());
    let terminal_retry = acquire(&mut controller, "owner", Some("session.input"), 1000, start);
    assert_eq!(terminal_retry, tombstone);
    assert_eq!(controller.metrics(start)["acquired"], 1);
}

#[test]
fn batches_record_canonical_events_and_release_all_held_input() {
    let now = Instant::now();
    let mut controller = Controller::new(true);
    let opened = acquire(&mut controller, "owner", Some("stream.input"), 5000, now);
    let token = lease(&opened).to_owned();
    let events = json!([
        {"kind":"key","key":{"platform":"moonlight_vk","code":65},"state":"down"},
        {"kind":"relative","dx":-300,"dy":700},
        {"kind":"absolute","x":0,"y":65535},
        {"kind":"button","button":"left","pressed":true}
    ]);
    let accepted = controller.handle(
        "owner",
        operation(json!({"action":"batch","lease":token,"events":events})),
        now,
    );
    assert_eq!(accepted["accepted"], 4);
    let metrics = controller.metrics(now);
    assert_eq!(metrics["events"], events);
    assert_eq!(metrics["held_keys"], 1);
    assert_eq!(metrics["held_buttons"], 1);

    let repeated = controller.handle(
        "owner",
        operation(json!({
            "action":"batch", "lease":token,
            "events":[{"kind":"key","key":{"platform":"moonlight_vk","code":65},"state":"repeat"}]
        })),
        now,
    );
    assert_eq!(repeated["accepted"], 1);
    let before_invalid = controller.metrics(now)["events"].as_array().unwrap().len();
    let duplicate = controller.handle(
        "owner",
        operation(json!({
            "action":"batch", "lease":token,
            "events":[{"kind":"key","key":{"platform":"moonlight_vk","code":65},"state":"down"}]
        })),
        now,
    );
    assert_eq!(error_code(&duplicate), Some("invalid_sequence"));
    assert_eq!(
        controller.metrics(now)["events"].as_array().unwrap().len(),
        before_invalid
    );

    let wrong_owner = controller.handle(
        "other",
        operation(json!({
            "action":"batch", "lease":token,
            "events":[{"kind":"relative","dx":1,"dy":1}]
        })),
        now,
    );
    assert_eq!(error_code(&wrong_owner), Some("permission_denied"));
    let released = controller.handle(
        "owner",
        operation(json!({"action":"release","lease":token})),
        now,
    );
    assert_eq!(released, json!({"ok":true,"released":true}));
    let metrics = controller.metrics(now);
    assert_eq!(metrics["active"], false);
    assert_eq!(metrics["released"], 1);
    assert_eq!(metrics["held_keys"], 0);
    assert_eq!(metrics["held_buttons"], 0);
    let tombstone = controller.handle(
        "owner",
        operation(json!({"action":"lookup","operation_id":"stream.input"})),
        now,
    );
    assert_eq!(tombstone["reason"], "released");
}

#[test]
fn renew_delays_expiry_and_expiry_retains_a_tombstone() {
    let start = Instant::now();
    let mut controller = Controller::new(true);
    let opened = acquire(
        &mut controller,
        "owner",
        Some("expiring.input"),
        1000,
        start,
    );
    let token = lease(&opened).to_owned();
    let renewed = controller.handle(
        "owner",
        operation(json!({"action":"renew","lease":token,"ttl_ms":1000})),
        start + Duration::from_millis(900),
    );
    assert_eq!(renewed["expires_in_ms"], 1000);
    controller.expire(start + Duration::from_millis(1500));
    assert_eq!(controller.metrics(start)["active"], true);
    controller.expire(start + Duration::from_millis(1900));
    let metrics = controller.metrics(start + Duration::from_millis(1900));
    assert_eq!(metrics["active"], false);
    assert_eq!(metrics["renewed"], 1);
    assert_eq!(metrics["expired"], 1);
    let tombstone = controller.handle(
        "owner",
        operation(json!({"action":"lookup","operation_id":"expiring.input"})),
        start + Duration::from_millis(1900),
    );
    assert_eq!(tombstone["reason"], "expired");
}

#[test]
fn validation_is_atomic_and_recording_is_bounded() {
    let now = Instant::now();
    let mut controller = Controller::new(true);
    let opened = acquire(&mut controller, "owner", None, 5000, now);
    let token = lease(&opened).to_owned();

    assert!(
        serde_json::from_value::<Operation>(json!({
            "action":"batch", "lease":token,
            "events":[{"kind":"absolute","x":65536,"y":2}]
        }))
        .is_err()
    );
    let invalid = controller.handle(
        "owner",
        operation(json!({
            "action":"batch", "lease":token,
            "events":[
                {"kind":"relative","dx":1,"dy":1},
                {"kind":"key","key":{"platform":"moonlight_vk","code":0},"state":"down"}
            ]
        })),
        now,
    );
    assert_eq!(error_code(&invalid), Some("invalid_event"));
    assert!(
        controller.metrics(now)["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    for batch in 0..5 {
        let count = if batch == 4 { 1 } else { 64 };
        let events = (0..count)
            .map(|index| json!({"kind":"relative","dx":batch * 64 + index + 1,"dy":-1}))
            .collect::<Vec<_>>();
        let accepted = controller.handle(
            "owner",
            operation(json!({"action":"batch","lease":token,"events":events})),
            now,
        );
        assert_eq!(accepted["accepted"], count);
    }
    let metrics = controller.metrics(now);
    assert_eq!(metrics["events"].as_array().unwrap().len(), 256);
    assert_eq!(metrics["events_truncated"], true);
}

#[test]
fn scoped_policy_is_observable_and_bound_to_the_acquisition_operation() {
    let now = Instant::now();
    let mut controller = Controller::new(true);
    let request = json!({"action":"acquire", "operation_id":"scoped-policy", "ttl_ms":1000,
        "high_resolution_scrolling":false, "always_send_scancodes":false,
        "key_rightalt_to_key_win":true, "key_repeat_delay":0, "key_repeat_frequency":0.0});
    let acquired = controller.handle("owner", operation(request.clone()), now);
    assert_eq!(acquired["ok"], true);
    let snapshot = controller.metrics(now)["requested_policy"].clone();
    for key in [
        "high_resolution_scrolling",
        "always_send_scancodes",
        "key_rightalt_to_key_win",
        "key_repeat_delay",
        "key_repeat_frequency",
    ] {
        assert_eq!(snapshot[key], request[key]);
    }
    assert!(snapshot["keyboard"].is_null());
    let retry = controller.handle("owner", operation(request.clone()), now);
    assert_eq!(lease(&retry), lease(&acquired));
    let mut changed = request;
    changed["key_repeat_delay"] = json!(null);
    assert_eq!(
        error_code(&controller.handle("owner", operation(changed), now)),
        Some("operation_conflict")
    );
    let mut invalid = json!({"action":"acquire", "key_repeat_frequency":-0.1});
    assert_eq!(
        error_code(&controller.handle("other", operation(invalid.clone()), now)),
        Some("invalid_request")
    );
    invalid["key_repeat_frequency"] = json!(null);
    invalid["key_repeat_delay"] = json!(2147483648_u64);
    assert_eq!(
        error_code(&controller.handle("other", operation(invalid), now)),
        Some("invalid_request")
    );
}

#[test]
fn invalid_gamepad_profiles_never_allocate_or_replace_the_observed_policy() {
    let now = Instant::now();
    let mut controller = Controller::new(true);
    for profile in ["unknown-profile".to_owned(), "x".repeat(20_000)] {
        let rejected = controller.handle(
            "owner",
            operation(json!({"action":"acquire", "gamepad":profile})),
            now,
        );
        assert_eq!(error_code(&rejected), Some("invalid_request"));
        assert_eq!(controller.metrics(now)["acquired"], 0);
        assert!(controller.metrics(now)["requested_policy"].is_null());
    }
    let acquired = controller.handle(
        "owner",
        operation(json!({"action":"acquire", "gamepad":"auto"})),
        now,
    );
    assert_eq!(acquired["ok"], true);
    assert_eq!(
        controller.metrics(now)["requested_policy"]["gamepad"],
        "auto"
    );
}

#[test]
fn keybindings_snapshot_preserves_order_empty_and_inherited_policy_without_emulation() {
    let now = Instant::now();
    for bindings in [json!(null), json!([]), json!([0, 255, 65, 66, 65, 67])] {
        let mut controller = Controller::new(true);
        let request =
            json!({"action":"acquire","operation_id":"keybindings","keybindings":bindings});
        let acquired = controller.handle("owner", operation(request.clone()), now);
        assert_eq!(acquired["ok"], true);
        let metrics = controller.metrics(now);
        assert_eq!(metrics["requested_policy"]["keybindings"], bindings);
        assert!(metrics.get("lease").is_none());
        assert!(metrics.get("token").is_none());
        assert!(metrics.get("operation_id").is_none());
        assert!(metrics.get("owner").is_none());
        let retry = controller.handle("owner", operation(request.clone()), now);
        assert_eq!(lease(&retry), lease(&acquired));
        let mut changed = request;
        changed["keybindings"] = if bindings.is_null() {
            json!([])
        } else {
            json!(null)
        };
        assert_eq!(
            error_code(&controller.handle("owner", operation(changed.clone()), now)),
            Some("operation_conflict")
        );
        let released = controller.handle(
            "owner",
            operation(json!({"action":"release","lease":lease(&acquired)})),
            now,
        );
        assert_eq!(released["ok"], true);
        assert_eq!(
            error_code(&controller.handle("owner", operation(changed), now)),
            Some("operation_conflict")
        );
        let lookup = controller.handle(
            "owner",
            operation(json!({"action":"lookup","operation_id":"keybindings"})),
            now,
        );
        assert_eq!(lookup["state"], "released");
        assert!(lookup["lease"].is_null());
        assert!(controller.metrics(now)["requested_policy"].is_null());
    }
}

#[test]
fn keybindings_omission_and_null_share_the_inherited_request_and_invalid_bytes_are_rejected() {
    let now = Instant::now();
    let mut controller = Controller::new(true);
    let acquired = controller.handle(
        "owner",
        operation(json!({"action":"acquire","operation_id":"inherit"})),
        now,
    );
    assert_eq!(acquired["ok"], true);
    let retry = controller.handle(
        "owner",
        operation(json!({"action":"acquire","operation_id":"inherit","keybindings":null})),
        now,
    );
    assert_eq!(lease(&retry), lease(&acquired));
    for invalid in [
        json!([0]),
        json!([-1, 0]),
        json!([0, 256]),
        json!([0.0, 1]),
        json!(["0", 1]),
        json!(vec![0_u8; 514]),
    ] {
        assert!(
            serde_json::from_value::<Operation>(json!({"action":"acquire","keybindings":invalid}))
                .is_err()
        );
    }
    assert!(
        serde_json::from_value::<Operation>(
            json!({"action":"acquire","keybindings":vec![0_u8;512]})
        )
        .is_ok()
    );
}
