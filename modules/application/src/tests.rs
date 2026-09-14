use crate::ProcessManager;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn launch(operation_id: &str, mode: &str) -> Value {
    json!({
        "action":"launch",
        "operation_id":operation_id,
        "executable":std::env::current_exe().unwrap().to_str().unwrap(),
        "args":["--exact","tests::controlled_child","--ignored"],
        "environment":{"AMATERASU_TEST_CHILD":mode}
    })
}

fn status(manager: &mut ProcessManager, caller: &str, id: u64) -> Value {
    manager.handle(caller, json!({"action":"status","process_id":id}))
}

#[test]
#[ignore = "subprocess entry point invoked by process lifecycle tests"]
fn controlled_child() {
    match std::env::var("AMATERASU_TEST_CHILD").as_deref() {
        Ok("sleep") => std::thread::sleep(Duration::from_secs(30)),
        Ok("exit") => {}
        Ok("tree") | Ok("tree_exit") | Ok("branch") | Ok("leaf") => tree_child(),
        #[cfg(unix)]
        Ok("graceful") => graceful_child(),
        Ok("cwd") => {
            assert_eq!(
                std::env::current_dir().unwrap(),
                std::path::PathBuf::from(std::env::var("AMATERASU_TEST_CWD").unwrap())
            );
        }
        _ => {}
    }
}

#[test]
fn launch_retry_conflict_ownership_and_termination() {
    let mut manager = ProcessManager::default();
    let request = launch("operation", "sleep");
    let first = manager.handle("alice", request.clone());
    assert_eq!(first["process"]["state"], "running", "{first}");
    let id = first["process"]["process_id"].as_u64().unwrap();
    assert_eq!(manager.handle("alice", request.clone()), first);
    assert_eq!(
        manager.handle("alice", launch("operation", "exit"))["code"],
        "operation_conflict"
    );
    assert_eq!(status(&mut manager, "bob", id)["code"], "process_not_found");
    assert_eq!(
        manager.handle("bob", json!({"action":"terminate","process_id":id}))["code"],
        "process_not_found"
    );
    assert_eq!(
        manager.handle("bob", json!({"action":"list"}))["processes"],
        json!([])
    );
    let ended = manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(ended["process"]["state"], "exited");
    assert_eq!(manager.handle("alice", request), ended);
    assert_eq!(
        manager.handle("alice", json!({"action":"terminate","process_id":id})),
        ended
    );
}

fn wait_for_exit(manager: &mut ProcessManager, id: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        manager.reap_exited().unwrap();
        let record = status(manager, "alice", id);
        if record["process"]["state"] == "exited" {
            return record;
        }
        assert!(Instant::now() < deadline, "child failed to exit: {record}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn tick_reaps_natural_exit_and_preserves_operation_record() {
    let mut manager = ProcessManager::default();
    let request = launch("natural-exit", "exit");
    let started = manager.handle("alice", request.clone());
    let id = started["process"]["process_id"].as_u64().unwrap();
    let exited = wait_for_exit(&mut manager, id);
    assert_eq!(exited["process"]["exit_code"], 0);
    assert_eq!(exited["process"]["success"], true);
    assert_eq!(manager.handle("alice", request), exited);
}

#[test]
fn explicit_environment_and_working_directory_reach_child() {
    let mut manager = ProcessManager::default();
    let mut request = launch("cwd", "cwd");
    request["cwd"] = json!(std::env::temp_dir().canonicalize().unwrap());
    request["environment"]["AMATERASU_TEST_CWD"] = request["cwd"].clone();
    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();
    assert_eq!(wait_for_exit(&mut manager, id)["process"]["success"], true);
}

#[test]
fn shutdown_terminates_and_reaps_every_direct_child() {
    let mut manager = ProcessManager::default();
    manager.handle("alice", launch("one", "sleep"));
    manager.handle("bob", launch("two", "sleep"));
    manager.shutdown().unwrap();
    assert_eq!(
        status(&mut manager, "alice", 1)["process"]["state"],
        "exited"
    );
    assert_eq!(status(&mut manager, "bob", 2)["process"]["state"], "exited");
    manager.shutdown().unwrap();
}

#[test]
fn failed_launch_is_retained_and_caller_scoped_capacity_is_bounded() {
    let mut manager = ProcessManager::default();
    let missing = std::env::temp_dir().join("amaterasu-nonexistent-executable-test");
    let request = json!({"action":"launch","operation_id":"missing","executable":missing});
    let failed = manager.handle("alice", request.clone());
    assert_eq!(failed["process"]["state"], "launch_failed");
    assert_eq!(manager.handle("alice", request.clone()), failed);
    assert_eq!(
        manager.handle("bob", request.clone())["process"]["process_id"],
        2
    );
    for index in 1..32 {
        let mut next = request.clone();
        next["operation_id"] = json!(format!("missing-{index}"));
        assert_eq!(manager.handle("alice", next)["ok"], true);
    }
    let mut over_limit = request;
    over_limit["operation_id"] = json!("over-limit");
    assert_eq!(manager.handle("alice", over_limit)["code"], "process_limit");
    let page = manager.handle("alice", json!({"action":"list","limit":16}));
    assert_eq!(page["processes"].as_array().unwrap().len(), 16);
    assert_eq!(page["next_offset"], 16);
    assert_eq!(
        manager.handle("alice", json!({"action":"list","offset":16}))["next_offset"],
        Value::Null
    );
}

#[test]
fn malformed_paths_arguments_environment_and_payload_are_rejected() {
    let mut manager = ProcessManager::default();
    let base = launch("validation", "exit");
    for (field, value) in [
        ("executable", json!("relative")),
        ("cwd", json!("relative")),
        (
            "executable",
            json!(std::env::temp_dir().join("implicit.CMD")),
        ),
        ("args", json!(["\u{0}"])),
        ("args", json!(vec!["arg"; 65])),
        ("environment", json!({"INVALID=KEY":"value"})),
        ("environment", json!({"Path":"one","PATH":"two"})),
        ("operation_id", json!("has spaces")),
    ] {
        let mut request = base.clone();
        request[field] = value;
        assert_eq!(
            manager.handle("alice", request)["code"],
            "invalid_launch",
            "{field}"
        );
    }
    let mut unknown = base.clone();
    unknown["shell"] = json!(true);
    assert_eq!(manager.handle("alice", unknown)["code"], "invalid_request");
    let mut oversized = base;
    oversized["args"] = json!(["a".repeat(16384)]);
    assert_eq!(
        manager.handle("alice", oversized)["code"],
        "request_too_large"
    );
    assert_eq!(
        manager.handle("alice", json!({"action":"list","limit":17}))["code"],
        "invalid_limit"
    );
    assert_eq!(
        manager.handle("alice", json!({"action":"list"}))["processes"],
        json!([])
    );
}

fn tree_child() {
    #[cfg(unix)]
    // SAFETY: SIG_IGN is a platform-defined signal disposition; deliberately
    // uncooperative descendants exercise the manager's forced-stop fallback.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    let mode = std::env::var("AMATERASU_TEST_CHILD").unwrap();
    let directory = std::path::PathBuf::from(std::env::var("AMATERASU_TEST_TREE").unwrap());
    if mode != "leaf" {
        let next = if mode == "branch" { "leaf" } else { "branch" };
        #[allow(clippy::zombie_processes)] // Orphaning is the behavior under test.
        let _child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::controlled_child", "--ignored"])
            .env("AMATERASU_TEST_CHILD", next)
            .env("AMATERASU_TEST_TREE", &directory)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Deliberately do not wait: the manager must own descendants itself.
    }
    if mode == "tree_exit" {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !directory.join("leaf").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        return;
    }
    let mut count = 0u64;
    loop {
        std::fs::write(directory.join(&mode), count.to_string()).unwrap();
        count += 1;
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn termination_and_natural_root_exit_stop_children_and_grandchildren() {
    for mode in ["tree", "tree_exit"] {
        let directory =
            std::env::temp_dir().join(format!("amaterasu-tree-{}-{mode}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let mut manager = ProcessManager::default();
        let mut request = launch(mode, mode);
        request["environment"]["AMATERASU_TEST_TREE"] = json!(directory);
        let launched = manager.handle("alice", request);
        assert_eq!(launched["process"]["state"], "running", "{launched}");
        let id = launched["process"]["process_id"].as_u64().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !directory.join("leaf").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        if mode == "tree" {
            let began = Instant::now();
            let terminated = manager.handle("alice", json!({"action":"terminate","process_id":id}));
            assert_eq!(terminated["ok"], true, "{terminated}");
            assert!(began.elapsed() < Duration::from_secs(3));
        } else {
            wait_for_exit(&mut manager, id);
        }
        // Observe behavior rather than checking a PID that could have been reused.
        std::thread::sleep(Duration::from_millis(100));
        let read = || ["branch", "leaf"].map(|name| std::fs::read(directory.join(name)).unwrap());
        let stopped = read();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(read(), stopped, "descendant kept running after {mode}");
        manager.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn shutdown_grace_is_shared_by_all_owned_trees() {
    let mut manager = ProcessManager::default();
    for index in 0..16 {
        assert_eq!(
            manager.handle("alice", launch(&format!("shutdown-{index}"), "sleep"))["process"]["state"],
            "running"
        );
    }
    let began = Instant::now();
    manager.shutdown().unwrap();
    assert!(began.elapsed() < Duration::from_secs(3));
}

#[cfg(unix)]
static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn request_test_stop(_: libc::c_int) {
    STOP_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(unix)]
fn graceful_child() {
    let path = std::path::PathBuf::from(std::env::var("AMATERASU_TEST_TREE").unwrap());
    // SAFETY: handler has the required C signature, performs only a lock-free
    // atomic store, and remains installed for this dedicated test child's life.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            request_test_stop as *const () as libc::sighandler_t,
        );
    }
    std::fs::write(path.join("ready"), b"ready").unwrap();
    while !STOP_REQUESTED.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(1));
    }
    std::fs::write(path.join("graceful"), b"stopped").unwrap();
}

#[test]
#[cfg(unix)]
fn cooperative_process_receives_graceful_stop_before_force() {
    let directory = std::env::temp_dir().join(format!("amaterasu-graceful-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let mut manager = ProcessManager::default();
    let mut request = launch("graceful", "graceful");
    request["environment"]["AMATERASU_TEST_TREE"] = json!(directory);
    let launched = manager.handle("alice", request);
    assert_eq!(launched["process"]["state"], "running", "{launched}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.join("ready").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let result = manager.handle("alice", json!({"action":"terminate","process_id":1}));
    assert_eq!(result["process"]["success"], true, "{result}");
    assert_eq!(
        std::fs::read(directory.join("graceful")).unwrap(),
        b"stopped"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn lookup_reconciles_owned_process_without_relaunching() {
    let mut manager = ProcessManager::default();
    let launched = manager.handle("owner", launch("reconcile", "sleep"));
    let request = json!({"action":"lookup","operation_id":"reconcile"});
    let found = manager.handle("owner", request.clone());
    assert_eq!(found["process"]["pid"], launched["process"]["pid"]);
    assert_eq!(
        manager.handle("foreign", request)["code"],
        "process_not_found"
    );
    assert_eq!(
        manager.handle("owner", json!({"action":"lookup","operation_id":"absent"}))["code"],
        "process_not_found"
    );
    let id = launched["process"]["process_id"].clone();
    assert_eq!(
        manager.handle("owner", json!({"action":"terminate","process_id":id}))["ok"],
        true
    );
}
