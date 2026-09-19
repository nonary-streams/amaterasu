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
        Ok("trace") | Ok("trace_fail") | Ok("trace_sleep") | Ok("trace_fail_once") => trace_child(),
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

fn trace_child() {
    use std::io::Write;

    let mode = std::env::var("AMATERASU_TEST_CHILD").unwrap();
    let trace = std::path::PathBuf::from(std::env::var("AMATERASU_TEST_TRACE").unwrap());
    let event = std::env::var("AMATERASU_TEST_EVENT").unwrap();
    let mut output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(trace)
        .unwrap();
    writeln!(output, "{event}").unwrap();
    drop(output);

    if mode == "trace_sleep" {
        std::thread::sleep(Duration::from_secs(30));
    }
    if mode == "trace_fail_once" {
        let marker = std::path::PathBuf::from(std::env::var("AMATERASU_TEST_MARKER").unwrap());
        if !marker.exists() {
            std::fs::write(marker, b"failed").unwrap();
            std::process::exit(3);
        }
    }
    if mode == "trace_fail" {
        std::process::exit(3);
    }
}

fn lifecycle_command(trace: &std::path::Path, event: &str, mode: &str) -> Value {
    let mut environment = json!({
        "AMATERASU_TEST_CHILD":mode,
        "AMATERASU_TEST_TRACE":trace,
        "AMATERASU_TEST_EVENT":event,
    });
    if mode == "trace_fail_once" {
        environment["AMATERASU_TEST_MARKER"] = json!(trace.with_extension("marker"));
    }
    json!({
        "executable":std::env::current_exe().unwrap(),
        "args":["--exact","tests::controlled_child","--ignored"],
        "environment":environment,
        "timeout_ms":if mode == "trace_sleep" { 30_000 } else { 5_000 },
    })
}

fn trace_events(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|contents| contents.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn test_directory(name: &str) -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-application-{}-{name}",
        std::process::id()
    ));
    if directory.exists() {
        std::fs::remove_dir_all(&directory).unwrap();
    }
    std::fs::create_dir(&directory).unwrap();
    directory
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
    let stopping = manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(stopping["process"]["state"], "stopping");
    let ended = wait_for_state(&mut manager, "alice", id, "cancelled");
    assert_eq!(manager.handle("alice", request), ended);
    assert_eq!(
        manager.handle("alice", json!({"action":"terminate","process_id":id})),
        ended
    );
}

fn wait_for_state(manager: &mut ProcessManager, caller: &str, id: u64, expected: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        manager.reap_exited().unwrap();
        let record = status(manager, caller, id);
        if record["process"]["state"] == expected {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "process failed to reach {expected}: {record}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_exit(manager: &mut ProcessManager, id: u64) -> Value {
    wait_for_state(manager, "alice", id, "exited")
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
        "cancelled"
    );
    assert_eq!(
        status(&mut manager, "bob", 2)["process"]["state"],
        "cancelled"
    );
    manager.shutdown().unwrap();
}

#[test]
fn shutdown_allows_prepared_process_undo_to_complete() {
    let directory = test_directory("shutdown-prepared");
    let trace = directory.join("events");
    let mut manager = ProcessManager::default();
    let mut request = launch("shutdown-prepared", "sleep");
    request["preparation"] = json!([{
        "run":lifecycle_command(&trace, "prepare", "trace"),
        "undo":lifecycle_command(&trace, "undo", "trace"),
    }]);

    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();
    wait_for_state(&mut manager, "alice", id, "running");
    assert_eq!(trace_events(&trace), ["prepare"]);

    manager.shutdown().unwrap();
    let cancelled = status(&mut manager, "alice", id);
    assert_eq!(cancelled["process"]["state"], "cancelled");
    assert_eq!(cancelled["process"]["cleanup"]["state"], "complete");
    assert_eq!(trace_events(&trace), ["prepare", "undo"]);
    manager.shutdown().unwrap();

    std::fs::remove_dir_all(directory).unwrap();
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

#[test]
fn preparation_runs_in_order_and_undo_runs_once_in_reverse() {
    let directory = test_directory("ordered-lifecycle");
    let trace = directory.join("events");
    let mut manager = ProcessManager::default();
    let mut request = launch("prepared", "sleep");
    request["preparation"] = json!([
        {
            "run":lifecycle_command(&trace, "prepare-one", "trace"),
            "undo":lifecycle_command(&trace, "undo-one", "trace"),
        },
        {
            "run":lifecycle_command(&trace, "prepare-two", "trace"),
            "undo":lifecycle_command(&trace, "undo-two", "trace"),
        },
    ]);

    let started = manager.handle("alice", request.clone());
    assert_eq!(started["process"]["state"], "preparing", "{started}");
    assert_eq!(started["process"]["pid"], Value::Null);
    assert_eq!(started["process"]["preparation"]["completed"], 0);
    assert_eq!(started["process"]["cleanup"]["state"], "deferred");
    let id = started["process"]["process_id"].as_u64().unwrap();

    let running = wait_for_state(&mut manager, "alice", id, "running");
    assert_eq!(running["process"]["preparation"]["completed"], 2);
    assert_eq!(running["process"]["cleanup"]["remaining"], 2);
    assert_eq!(trace_events(&trace), ["prepare-one", "prepare-two"]);

    let repeated = manager.handle("alice", request.clone());
    assert_eq!(repeated["process"]["process_id"], id);
    assert_eq!(repeated["process"]["state"], "running");
    assert_eq!(trace_events(&trace), ["prepare-one", "prepare-two"]);
    let mut conflicting = request.clone();
    conflicting["preparation"][0]["run"] = lifecycle_command(&trace, "different-prepare", "trace");
    assert_eq!(
        manager.handle("alice", conflicting)["code"],
        "operation_conflict"
    );

    let stopping = manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(stopping["process"]["state"], "stopping", "{stopping}");
    let cancelled = wait_for_state(&mut manager, "alice", id, "cancelled");
    assert_eq!(cancelled["process"]["cleanup"]["state"], "complete");
    assert_eq!(cancelled["process"]["cleanup"]["remaining"], 0);
    assert_eq!(
        trace_events(&trace),
        ["prepare-one", "prepare-two", "undo-two", "undo-one"]
    );

    assert_eq!(
        manager.handle("alice", request)["process"]["state"],
        "cancelled"
    );
    assert_eq!(
        manager.handle("alice", json!({"action":"terminate","process_id":id}))["process"]["state"],
        "cancelled"
    );
    assert_eq!(
        trace_events(&trace),
        ["prepare-one", "prepare-two", "undo-two", "undo-one"]
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn prepared_natural_exit_and_application_spawn_failure_both_undo() {
    let directory = test_directory("prepared-outcomes");
    let mut manager = ProcessManager::default();

    let natural_trace = directory.join("natural-events");
    let mut natural_request = launch("prepared-natural", "exit");
    natural_request["preparation"] = json!([{
        "run":lifecycle_command(&natural_trace, "prepare", "trace"),
        "undo":lifecycle_command(&natural_trace, "undo", "trace"),
    }]);
    let natural = manager.handle("alice", natural_request);
    let natural_id = natural["process"]["process_id"].as_u64().unwrap();
    let exited = wait_for_state(&mut manager, "alice", natural_id, "exited");
    assert_eq!(exited["process"]["success"], true);
    assert_eq!(exited["process"]["cleanup"]["state"], "complete");
    assert_eq!(trace_events(&natural_trace), ["prepare", "undo"]);

    let failed_trace = directory.join("failed-events");
    let mut failed_request = launch("prepared-spawn-failure", "exit");
    failed_request["executable"] = json!(directory.join("missing-application"));
    failed_request["preparation"] = json!([{
        "run":lifecycle_command(&failed_trace, "prepare", "trace"),
        "undo":lifecycle_command(&failed_trace, "undo", "trace"),
    }]);
    let failed = manager.handle("alice", failed_request);
    let failed_id = failed["process"]["process_id"].as_u64().unwrap();
    let failed = wait_for_state(&mut manager, "alice", failed_id, "launch_failed");
    assert_eq!(failed["process"]["pid"], Value::Null);
    assert_eq!(failed["process"]["preparation"]["completed"], 1);
    assert_eq!(failed["process"]["cleanup"]["state"], "complete");
    assert_eq!(trace_events(&failed_trace), ["prepare", "undo"]);

    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn failed_preparation_rolls_back_the_started_step_and_prior_steps() {
    let directory = test_directory("failed-preparation");
    let trace = directory.join("events");
    let mut manager = ProcessManager::default();
    let mut request = launch("failed-preparation", "sleep");
    request["preparation"] = json!([
        {
            "run":lifecycle_command(&trace, "prepare-one", "trace"),
            "undo":lifecycle_command(&trace, "undo-one", "trace"),
        },
        {
            "run":lifecycle_command(&trace, "prepare-two", "trace_fail"),
            "undo":lifecycle_command(&trace, "undo-two", "trace"),
        },
    ]);

    let started = manager.handle("alice", request.clone());
    let id = started["process"]["process_id"].as_u64().unwrap();
    let failed = wait_for_state(&mut manager, "alice", id, "launch_failed");
    assert_eq!(failed["process"]["pid"], Value::Null);
    assert_eq!(failed["process"]["preparation"]["completed"], 1);
    assert_eq!(
        failed["process"]["preparation"]["failure"]["kind"],
        "exit_failed"
    );
    assert_eq!(failed["process"]["preparation"]["failure"]["step"], 1);
    assert_eq!(failed["process"]["cleanup"]["state"], "complete");
    assert_eq!(
        trace_events(&trace),
        ["prepare-one", "prepare-two", "undo-two", "undo-one"]
    );

    assert_eq!(manager.handle("alice", request), failed);
    assert_eq!(
        trace_events(&trace),
        ["prepare-one", "prepare-two", "undo-two", "undo-one"]
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cancellation_settles_an_active_preparation_before_undo() {
    let directory = test_directory("cancel-preparation");
    let trace = directory.join("events");
    let mut manager = ProcessManager::default();
    let mut request = launch("cancel-preparation", "sleep");
    request["preparation"] = json!([{
        "run":lifecycle_command(&trace, "prepare-long", "trace_sleep"),
        "undo":lifecycle_command(&trace, "undo-long", "trace"),
    }]);

    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while trace_events(&trace).is_empty() {
        let tick_started = Instant::now();
        manager.reap_exited().unwrap();
        assert!(
            tick_started.elapsed() < Duration::from_millis(500),
            "a lifecycle tick blocked on a long command"
        );
        assert!(
            Instant::now() < deadline,
            "preparation command did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let stopping = manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(stopping["process"]["state"], "stopping", "{stopping}");
    let cancelled = wait_for_state(&mut manager, "alice", id, "cancelled");
    assert_eq!(cancelled["process"]["preparation"]["completed"], 0);
    assert_eq!(
        cancelled["process"]["preparation"]["failure"]["kind"],
        "cancelled"
    );
    assert_eq!(trace_events(&trace), ["prepare-long", "undo-long"]);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn cleanup_failure_is_retained_until_explicit_retry_succeeds() {
    let directory = test_directory("retry-cleanup");
    let trace = directory.join("events");
    let mut manager = ProcessManager::default();
    let mut request = launch("retry-cleanup", "sleep");
    request["preparation"] = json!([{
        "run":lifecycle_command(&trace, "prepare", "trace"),
        "undo":lifecycle_command(&trace, "undo-attempt", "trace_fail_once"),
    }]);

    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();
    wait_for_state(&mut manager, "alice", id, "running");
    manager.handle("alice", json!({"action":"terminate","process_id":id}));
    let failed = wait_for_state(&mut manager, "alice", id, "cleanup_failed");
    assert_eq!(failed["process"]["outcome"], "cancelled");
    assert_eq!(failed["process"]["cleanup"]["state"], "failed");
    assert_eq!(failed["process"]["cleanup"]["remaining"], 1);
    assert_eq!(failed["process"]["cleanup"]["failures"][0]["phase"], "undo");
    assert_eq!(
        failed["process"]["cleanup"]["failures"][0]["kind"],
        "exit_failed"
    );
    assert_eq!(trace_events(&trace), ["prepare", "undo-attempt"]);

    for _ in 0..3 {
        assert_eq!(status(&mut manager, "alice", id), failed);
    }
    assert_eq!(trace_events(&trace), ["prepare", "undo-attempt"]);

    let retry = manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(retry["process"]["state"], "stopping", "{retry}");
    let cancelled = wait_for_state(&mut manager, "alice", id, "cancelled");
    assert_eq!(cancelled["process"]["cleanup"]["state"], "complete");
    assert_eq!(
        trace_events(&trace),
        ["prepare", "undo-attempt", "undo-attempt"]
    );

    manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(
        trace_events(&trace),
        ["prepare", "undo-attempt", "undo-attempt"]
    );
    std::fs::remove_dir_all(directory).unwrap();
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
            assert_eq!(terminated["process"]["state"], "stopping", "{terminated}");
            assert!(began.elapsed() < Duration::from_secs(3));
            wait_for_state(&mut manager, "alice", id, "cancelled");
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
    assert_eq!(result["process"]["state"], "stopping", "{result}");
    let result = wait_for_state(&mut manager, "alice", 1, "cancelled");
    assert_eq!(result["process"]["state"], "cancelled", "{result}");
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
