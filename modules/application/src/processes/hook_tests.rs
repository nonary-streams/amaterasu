use super::*;

fn hook_launch(operation_id: &str, output: Option<&str>) -> Value {
    let fixture = match operation_id {
        "hook-ready" => 0,
        "hook-cancel" => 1,
        "hook-output-failure" => 2,
        _ => 3,
    };
    json!({
        "action": "launch",
        "operation_id": operation_id,
        "executable": "/bin/true",
        "steam_id": format!("{}", 4_000_000_000u64 + u64::from(std::process::id()) * 4 + fixture),
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
fn output_failure_cannot_resume_after_helper_becomes_ready() {
    let mut manager = ProcessManager::default();
    let missing = format!(
        "/amaterasu-no-such-output-parent-{}/log",
        std::process::id()
    );
    let response = manager.handle("alice", hook_launch("hook-output-failure", Some(&missing)));
    let id = response["process"]["process_id"].as_u64().unwrap();
    assert!(matches!(
        manager.records[&id].state,
        ProcessState::LaunchFailed { .. }
    ));
    assert!(!manager.records[&id].hook_pending);
    for _ in 0..20 {
        manager.reap_exited().unwrap();
    }
    assert!(manager.records[&id].pid.is_none());
}

#[test]
fn pending_hook_launch_fails_after_its_own_deadline() {
    let mut manager = ProcessManager::default();
    let response = manager.handle("alice", hook_launch("hook-timeout", None));
    let id = response["process"]["process_id"].as_u64().unwrap();
    manager.records.get_mut(&id).unwrap().hook_deadline = Some(Instant::now());
    manager.refresh_hook_readiness();
    assert!(matches!(
        manager.records[&id].state,
        ProcessState::LaunchFailed { .. }
    ));
    assert!(manager.records[&id].pid.is_none());
}
