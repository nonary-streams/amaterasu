use super::*;

fn mode(id: &str, width: u32, height: u32, refresh_rate: f64) -> Value {
    json!({
        "id": id,
        "size": { "width": width, "height": height },
        "refreshRate": refresh_rate
    })
}

fn output(
    name: &str,
    connected: bool,
    enabled: bool,
    current_mode: &str,
    pos: (i32, i32),
    modes: Vec<Value>,
) -> Value {
    json!({
        "name": name,
        "description": format!("{name} monitor"),
        "connected": connected,
        "enabled": enabled,
        "currentModeId": current_mode,
        "modes": modes,
        "pos": { "x": pos.0, "y": pos.1 },
        "rotation": 1,
        "scale": 1.25,
        "priority": if connected && enabled { if pos == (0, 0) { 1 } else { 2 } } else { 0 },
        "hdr": false
    })
}

fn reference_kde_configuration() -> Value {
    // Field shape and representative values match the DP-1/DP-2 KScreen
    // restore-policy fixture in Vibeshine's Linux test suite.
    let mut first = output(
        "DP-1",
        true,
        true,
        "25",
        (0, 0),
        vec![mode("25", 2560, 1440, 60.0)],
    );
    first["size"] = json!({ "width": 2560, "height": 1440 });
    first["scale"] = json!(1.5);
    first["rotation"] = json!(1);
    first["hdr"] = json!(true);
    let mut second = output(
        "DP-2",
        true,
        true,
        "31",
        (2560, 0),
        vec![mode("31", 1920, 1080, 60.0)],
    );
    second["size"] = json!({ "width": 1920, "height": 1080 });
    second["scale"] = json!(1.0);
    second["rotation"] = json!(2);
    second["hdr"] = json!(false);
    json!({ "outputs": [first, second] })
}

fn configuration() -> Value {
    json!({
        "outputs": [
            output(
                "DP-1",
                true,
                true,
                "1920x1080@60",
                (0, 0),
                vec![mode("1920x1080@60", 1920, 1080, 60.0), mode("1280x720@60", 1280, 720, 60.0)]
            ),
            output(
                "HDMI-A-1",
                true,
                false,
                "",
                (1920, 0),
                vec![mode("1920x1080@60", 1920, 1080, 60.0)]
            ),
            output("DP-3", false, false, "", (0, 0), vec![])
        ]
    })
}

#[test]
fn read_only_enumeration_and_snapshot_preserve_user_session_topology() {
    let configuration = configuration();
    let rows = enumeration_rows(&configuration, None).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "DP-1");
    assert_eq!(rows[0]["active"], true);
    assert_eq!(
        rows[0]["current"]["refresh"],
        json!({"numerator":60,"denominator":1})
    );
    assert_eq!(rows[1]["connected"], true);
    assert_eq!(rows[1]["active"], false);

    let modes = enumeration_rows(&configuration, Some("DP-1")).unwrap();
    assert_eq!(modes.len(), 2);
    assert_eq!(modes[0]["width"], 1920);
    assert_eq!(modes[0]["refresh"], json!({"numerator":60,"denominator":1}));

    let snapshot = configuration_snapshot(&configuration, "linux_kscreen").unwrap();
    assert_eq!(
        snapshot["configuration"]["displays"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        snapshot["session_state"]["outputs"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(snapshot["topology_revision"].as_str().unwrap().len(), 32);
}

#[test]
fn topology_revision_is_independent_of_kscreen_output_array_order() {
    let mut first = configuration();
    let mut second = first.clone();
    second["outputs"].as_array_mut().unwrap().reverse();
    let first_snapshot = configuration_snapshot(&first, "linux_kscreen").unwrap();
    let second_snapshot = configuration_snapshot(&second, "linux_kscreen").unwrap();
    assert_eq!(
        first_snapshot["topology_revision"],
        second_snapshot["topology_revision"]
    );
    assert_eq!(
        first_snapshot["configuration"],
        second_snapshot["configuration"]
    );

    first["outputs"][0]["scale"] = json!(1.5);
    let changed_snapshot = configuration_snapshot(&first, "linux_kscreen").unwrap();
    assert_ne!(
        first_snapshot["topology_revision"],
        changed_snapshot["topology_revision"]
    );
}

#[test]
fn kscreen_snapshot_retains_reference_output_identity_mode_and_layout_fields() {
    let raw = reference_kde_configuration();
    let snapshot = configuration_snapshot(&raw, "linux_kscreen").unwrap();
    let saved: KScreenSnapshot = serde_json::from_value(snapshot["session_state"].clone()).unwrap();
    assert_eq!(saved.version, 1);
    assert_eq!(saved.outputs.len(), 2);
    assert_eq!(saved.outputs[0].id, "DP-1");
    assert_eq!(saved.outputs[0].mode.as_ref().unwrap().id, "25");
    assert_eq!(saved.outputs[0].mode.as_ref().unwrap().width, 2560);
    assert_eq!(saved.outputs[0].position.x, 0);
    assert_eq!(saved.outputs[0].scale, 1.5);
    assert_eq!(saved.outputs[0].rotation, 1);
    assert!(saved.outputs[0].primary);
    assert_eq!(saved.outputs[0].hdr_enabled, Some(true));
    assert_eq!(saved.outputs[1].priority, 2);
    assert!(!saved.outputs[1].primary);
}

#[test]
fn restore_plan_rebinds_exact_outputs_and_reselects_unique_equivalent_mode() {
    let saved: KScreenSnapshot = serde_json::from_value(
        configuration_snapshot(&configuration(), "linux_kscreen").unwrap()["session_state"].clone(),
    )
    .unwrap();
    let mut current = configuration();
    current["outputs"][0]["currentModeId"] = json!("1280x720@60");
    current["outputs"][0]["modes"] = json!([
        mode("current-1920x1080", 1920, 1080, 60.0),
        mode("1280x720@60", 1280, 720, 60.0)
    ]);
    let arguments = restoration_arguments(&saved, &current).unwrap();
    assert!(arguments.contains(&"output.DP-1.enable".to_owned()));
    assert!(arguments.contains(&"output.DP-1.mode.current-1920x1080".to_owned()));
    assert!(arguments.contains(&"output.DP-1.position.0,0".to_owned()));
    assert!(arguments.contains(&"output.HDMI-A-1.disable".to_owned()));
}

#[test]
fn restore_plan_holds_on_disconnect_new_output_or_ambiguous_mode() {
    let saved: KScreenSnapshot = serde_json::from_value(
        configuration_snapshot(&configuration(), "linux_kscreen").unwrap()["session_state"].clone(),
    )
    .unwrap();
    let mut disconnected = configuration();
    disconnected["outputs"][0]["connected"] = json!(false);
    disconnected["outputs"][0]["enabled"] = json!(false);
    assert_eq!(
        restoration_arguments(&saved, &disconnected)
            .unwrap_err()
            .code,
        "topology_changed"
    );

    let mut added = configuration();
    added["outputs"].as_array_mut().unwrap().push(output(
        "DP-2",
        true,
        true,
        "1920x1080@60",
        (1920, 0),
        vec![mode("1920x1080@60", 1920, 1080, 60.0)],
    ));
    assert_eq!(
        restoration_arguments(&saved, &added).unwrap_err().code,
        "topology_changed"
    );

    let mut ambiguous = configuration();
    ambiguous["outputs"][0]["modes"] = json!([
        mode("alternative-a", 1920, 1080, 60.0),
        mode("alternative-b", 1920, 1080, 60.0),
        mode("1280x720@60", 1280, 720, 60.0)
    ]);
    ambiguous["outputs"][0]["currentModeId"] = json!("alternative-a");
    assert_eq!(
        restoration_arguments(&saved, &ambiguous).unwrap_err().code,
        "topology_changed"
    );

    let mut reconnected_baseline_output = configuration();
    reconnected_baseline_output["outputs"][2] = output(
        "DP-3",
        true,
        false,
        "",
        (0, 0),
        vec![mode("1920x1080@60", 1920, 1080, 60.0)],
    );
    assert_eq!(
        restoration_arguments(&saved, &reconnected_baseline_output)
            .unwrap_err()
            .code,
        "topology_changed"
    );
}

#[test]
fn malformed_or_unsafe_kscreen_id_fails_closed() {
    let mut malformed = configuration();
    malformed["outputs"][0]["name"] = json!("DP-1;disable-eDP-1");
    assert_eq!(
        validated_outputs(&malformed).unwrap_err().code,
        "platform_error"
    );
}

#[test]
fn linux_mutation_contract_remains_unsupported() {
    let mut platform = LinuxPlatform;
    assert!(!platform.supported());
    assert_eq!(
        platform.snapshot().unwrap_err().code,
        "unsupported_platform"
    );

    let state = configuration_snapshot(&reference_kde_configuration(), "linux_kscreen").unwrap();
    let saved: KScreenSnapshot = serde_json::from_value(state["session_state"].clone()).unwrap();
    let mut guard_called = false;
    let error = platform
        .restore_kscreen_snapshot(&saved, &mut || {
            guard_called = true;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.code, "unsupported_platform");
    assert!(!guard_called);
}
