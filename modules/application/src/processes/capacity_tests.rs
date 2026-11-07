use super::*;

fn preparation(operation_id: &str) -> Value {
    // A reached no-op step holds a real preparation lease without running a child.
    json!({"action":"prepare","operation_id":operation_id,"preparation":[{}]})
}

fn launch(operation_id: &str) -> Value {
    json!({
        "action":"launch",
        "operation_id":operation_id,
        "executable":std::env::current_exe().unwrap().join("missing-application"),
    })
}

fn admit(manager: &mut ProcessManager, caller: &str, request: Value) -> Value {
    let response = manager.handle(caller, request);
    assert_eq!(response["ok"], true, "{response}");
    response
}

#[test]
fn preparation_leases_bound_new_launches_per_caller() {
    let mut manager = ProcessManager::default();
    for index in 0..32 {
        admit(&mut manager, "alice", preparation(&format!("prep-{index}")));
    }

    let rejected = manager.handle("alice", launch("over-caller-limit"));
    assert_eq!(rejected["code"], "process_limit", "{rejected}");
    assert!(manager.records.is_empty());
    assert_eq!(manager.next_id, 0);
    admit(&mut manager, "bob", launch("other-caller"));
}

#[test]
fn preparation_leases_bound_new_launches_globally() {
    let mut manager = ProcessManager::default();
    for caller in ["alice", "bob", "carol", "dave"] {
        for index in 0..32 {
            admit(&mut manager, caller, preparation(&format!("prep-{index}")));
        }
    }

    let rejected = manager.handle("eve", launch("over-global-limit"));
    assert_eq!(rejected["code"], "process_limit", "{rejected}");
    assert!(manager.records.is_empty());
    assert_eq!(manager.next_id, 0);
}

#[test]
fn mixed_launches_and_preparations_share_caller_capacity_in_either_order() {
    for preparations_first in [false, true] {
        let mut manager = ProcessManager::default();
        for first in [true, false] {
            for index in 0..16 {
                let operation_id = format!("entry-{first}-{index}");
                let request = if first == preparations_first {
                    preparation(&operation_id)
                } else {
                    launch(&operation_id)
                };
                admit(&mut manager, "alice", request);
            }
        }

        for request in [launch("launch-over-limit"), preparation("prep-over-limit")] {
            let rejected = manager.handle("alice", request);
            assert_eq!(rejected["code"], "process_limit", "{rejected}");
        }
        assert_eq!(manager.records.len(), 16);
        assert_eq!(manager.preparations.len(), 16);
    }
}

#[test]
fn mixed_launches_and_preparations_share_global_capacity() {
    let mut manager = ProcessManager::default();
    for caller in 0..8 {
        let caller = format!("caller-{caller}");
        for index in 0..8 {
            admit(&mut manager, &caller, preparation(&format!("prep-{index}")));
            admit(&mut manager, &caller, launch(&format!("launch-{index}")));
        }
    }

    for request in [launch("launch-over-limit"), preparation("prep-over-limit")] {
        let rejected = manager.handle("new-caller", request);
        assert_eq!(rejected["code"], "process_limit", "{rejected}");
    }
    assert_eq!(manager.records.len(), 64);
    assert_eq!(manager.preparations.len(), 64);
}

#[test]
fn idempotent_launch_and_preparation_replays_survive_full_capacity() {
    let mut manager = ProcessManager::default();
    let launch_request = launch("existing-launch");
    let launch_response = admit(&mut manager, "alice", launch_request.clone());
    assert_eq!(launch_response["process"]["state"], "launch_failed");
    let preparation_request = preparation("existing-preparation");
    let preparation_response = admit(&mut manager, "alice", preparation_request.clone());
    for index in 0..30 {
        admit(&mut manager, "alice", preparation(&format!("prep-{index}")));
    }
    for caller in ["bob", "carol", "dave"] {
        for index in 0..32 {
            admit(&mut manager, caller, preparation(&format!("prep-{index}")));
        }
    }

    assert_eq!(manager.handle("alice", launch_request), launch_response);
    assert_eq!(
        manager.handle("alice", preparation_request),
        preparation_response
    );
    assert_eq!(manager.records.len(), 1);
    assert_eq!(manager.preparations.len(), 127);
}

#[test]
fn rejected_launch_does_not_open_output_or_create_a_process() {
    let mut manager = ProcessManager::default();
    for index in 0..32 {
        admit(&mut manager, "alice", preparation(&format!("prep-{index}")));
    }
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-capacity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir(&directory).unwrap();
    let output = directory.join("must-not-open");
    let mut request = launch("over-limit-output");
    request["output"] = json!(output);
    let rejected = manager.handle("alice", request);
    let output_exists = output.exists();
    std::fs::remove_dir_all(directory).unwrap();

    assert_eq!(rejected["code"], "process_limit", "{rejected}");
    assert!(
        !output_exists,
        "capacity rejection opened application output"
    );
    assert!(manager.records.is_empty());
    assert_eq!(manager.next_id, 0);
}

#[cfg(target_os = "linux")]
#[test]
fn rejected_launch_does_not_acquire_a_proton_hook() {
    let mut manager = ProcessManager::default();
    // Disabled hooks carry policy ownership but start no helper or Steam work.
    let disabled_hook = LaunchSpec {
        output: None,
        executable: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: Vec::new(),
        environment: BTreeMap::new(),
        cwd: None,
        preparation_cwd: None,
        detached_cwd: Default::default(),
        elevated: false,
        auto_detach: true,
        wait_all: true,
        exit_timeout_seconds: 0,
        steam_id: Some("42".into()),
        proton_dualsense_compatibility: false,
        steam_install_dir: None,
        detached: Vec::new(),
        preparation: Vec::new(),
    };
    assert_eq!(
        manager.proton_hooks.acquire(&disabled_hook),
        Ok(Some("42".into()))
    );
    for index in 0..32 {
        admit(&mut manager, "alice", preparation(&format!("prep-{index}")));
    }
    let mut request = launch("over-limit-proton");
    request["steam_id"] = json!("42");
    request["proton_dualsense_compatibility"] = json!(true);

    let rejected = manager.handle("alice", request);
    assert_eq!(rejected["code"], "process_limit", "{rejected}");
    assert!(manager.records.is_empty());
    manager.proton_hooks.release("42");
    assert!(manager.proton_hooks.shutdown_complete());
}
