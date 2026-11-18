use super::*;

fn hook_launch(operation_id: &str, output: Option<&str>) -> Value {
    let fixture = match operation_id {
        "hook-ready" => 0,
        "hook-cancel" => 1,
        "hook-output-failure" => 2,
        "hook-timeout" => 3,
        "hook-first" => 4,
        "hook-enabled-first" => 5,
        _ => 6,
    };
    json!({
        "action": "launch",
        "operation_id": operation_id,
        "executable": "/bin/true",
        "steam_id": format!("{}", 4_000_000_000u64 + u64::from(std::process::id()) * 8 + fixture),
        "proton_dualsense_compatibility": true,
        "output": output,
    })
}

#[test]
fn launch_waits_for_helper_readiness_without_blocking() {
    let mut manager = ProcessManager::default();
    let start = Instant::now();
    let response = manager.handle("alice", hook_launch("hook-ready", None));
    assert!(start.elapsed() < Duration::from_secs(1));
    let id = response["process"]["process_id"].as_u64().unwrap();
    assert_eq!(response["process"]["state"], "preparing");
    assert!(manager.records[&id].hook_pending);
    assert!(manager.records[&id].pid.is_none());

    let deadline = Instant::now() + Duration::from_secs(3);
    while manager.records[&id].pid.is_none() {
        manager.reap_exited().unwrap();
        assert!(Instant::now() < deadline, "helper did not become ready");
        std::thread::yield_now();
    }
}

#[test]
fn cancellation_while_helper_is_pending_never_spawns_the_application() {
    let mut manager = ProcessManager::default();
    let response = manager.handle("alice", hook_launch("hook-cancel", None));
    let id = response["process"]["process_id"].as_u64().unwrap();
    let record = manager.records.get_mut(&id).unwrap();
    assert!(record.hook_pending);
    record.request_termination().unwrap();
    assert!(matches!(record.state, ProcessState::Cancelled));
    manager.reap_exited().unwrap();
    assert!(manager.records[&id].pid.is_none());
}

#[test]
fn output_failure_warns_and_launches_after_helper_becomes_ready() {
    let mut manager = ProcessManager::default();
    let missing = format!(
        "/amaterasu-no-such-output-parent-{}/log",
        std::process::id()
    );
    let response = manager.handle("alice", hook_launch("hook-output-failure", Some(&missing)));
    let id = response["process"]["process_id"].as_u64().unwrap();
    assert_eq!(
        response["process"]["warnings"][0]["reason"],
        "output_open_failed"
    );
    assert!(manager.records[&id].hook_pending);
    let deadline = Instant::now() + Duration::from_secs(3);
    while manager.records[&id].pid.is_none() {
        manager.reap_exited().unwrap();
        assert!(Instant::now() < deadline, "helper did not become ready");
        std::thread::yield_now();
    }
    assert!(!manager.records[&id].hook_pending);
}

#[test]
fn pending_optional_hook_warns_and_launches_after_its_own_deadline() {
    let mut manager = ProcessManager::default();
    let response = manager.handle("alice", hook_launch("hook-timeout", None));
    let id = response["process"]["process_id"].as_u64().unwrap();
    manager.records.get_mut(&id).unwrap().hook_deadline = Some(Instant::now());
    manager.refresh_hook_readiness();
    let record = &manager.records[&id];
    assert!(record.pid.is_some());
    assert_eq!(record.warnings[0].reason, "proton_hook_unconfirmed");
    assert!(record.steam_proton_hook_app_id.is_some());
}

#[test]
fn enabled_hook_policy_conflict_warns_and_keeps_the_application_launch() {
    let mut manager = ProcessManager::default();
    let mut first = hook_launch("hook-first", None);
    first["proton_dualsense_compatibility"] = json!(false);
    let steam_id = first["steam_id"].clone();
    assert_eq!(manager.handle("alice", first)["ok"], true);

    let mut second = hook_launch("hook-second", None);
    second["steam_id"] = steam_id;
    let response = manager.handle("alice", second);
    assert_eq!(response["ok"], true);
    assert_eq!(
        response["process"]["warnings"][0]["reason"],
        "proton_hook_policy_conflict"
    );
    assert!(response["process"]["pid"].is_number());
}

#[test]
fn disabled_hook_policy_does_not_bypass_an_enabled_owner() {
    let mut manager = ProcessManager::default();
    let first = hook_launch("hook-enabled-first", None);
    let steam_id = first["steam_id"].clone();
    assert_eq!(manager.handle("alice", first)["ok"], true);

    let mut second = hook_launch("hook-disabled-second", None);
    second["steam_id"] = steam_id;
    second["proton_dualsense_compatibility"] = json!(false);
    let response = manager.handle("alice", second);
    assert_eq!(response["ok"], false);
    assert_eq!(response["code"], "proton_hook_policy_conflict");
}
