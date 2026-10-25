use crate::ProcessManager;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn launch(operation_id: &str, mode: &str) -> Value {
    json!({
        "action":"launch",
        "operation_id":operation_id,
        "executable":std::env::current_exe().unwrap().to_str().unwrap(),
        "args":["--exact","tests::controlled_child","--ignored"],
        "environment":{"AMATERASU_TEST_CHILD":mode},
        "auto_detach":false
    })
}

fn status(manager: &mut ProcessManager, caller: &str, id: u64) -> Value {
    manager.handle(caller, json!({"action":"status","process_id":id}))
}

fn wait_for_preparation_state(
    manager: &mut ProcessManager,
    caller: &str,
    operation_id: &str,
    state: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = manager.handle(
            caller,
            json!({"action":"preparation_status","operation_id":operation_id}),
        );
        if response["preparation"]["state"] == state {
            return response;
        }
        assert_ne!(response["ok"], false, "{response}");
        assert!(
            Instant::now() < deadline,
            "preparation did not reach {state}: {response}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "subprocess entry point invoked by process lifecycle tests"]
fn controlled_child() {
    let arguments = std::env::args().collect::<Vec<_>>();
    if let Some(label) = arguments
        .iter()
        .find_map(|argument| argument.strip_prefix("--application-output="))
    {
        use std::io::Write;
        writeln!(std::io::stdout(), "OUT-{label}").unwrap();
        writeln!(std::io::stderr(), "ERR-{label}").unwrap();
        return;
    }
    if let Some(argument_index) = arguments
        .iter()
        .position(|argument| argument == "--detached-arguments")
    {
        let marker = std::env::var("AMATERASU_TEST_MARKER").unwrap();
        let expanded_arguments = arguments[argument_index + 1..].join("\n");
        std::fs::write(marker, expanded_arguments).unwrap();
        return;
    }

    if arguments
        .iter()
        .any(|argument| argument == "--raw-preparation-marker")
    {
        use std::io::Write;

        let marker = std::env::current_dir()
            .unwrap()
            .join("raw-preparation-marker");
        let mut output = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(marker)
            .unwrap();
        writeln!(output, "raw").unwrap();
        return;
    }

    let mode = if arguments
        .iter()
        .any(|argument| argument == "--detached-child")
    {
        "detached-marker".into()
    } else {
        std::env::var("AMATERASU_TEST_CHILD").unwrap_or_default()
    };
    match mode.as_str() {
        "sleep" => std::thread::sleep(Duration::from_secs(30)),
        "exit" => {
            if let Ok(marker) = std::env::var("AMATERASU_TEST_MARKER") {
                std::fs::write(marker, b"exit").unwrap();
            }
        }
        "exit-failure" => std::process::exit(7),
        "detached-marker" | "steam-marker" => {
            std::thread::sleep(Duration::from_millis(100));
            let marker = std::env::var("AMATERASU_TEST_MARKER").unwrap();
            std::fs::write(marker, b"detached").unwrap();
        }
        "detached-output" => {
            use std::io::Write;
            let value = std::env::var("AMATERASU_TEST_VALUE").unwrap();
            writeln!(std::io::stdout(), "OUT-{value}").unwrap();
            writeln!(std::io::stderr(), "ERR-{value}").unwrap();
        }
        "application-output-main" => {
            use std::io::Write;
            writeln!(std::io::stdout(), "OUT-main").unwrap();
            writeln!(std::io::stderr(), "ERR-main").unwrap();
            if let Ok(marker) = std::env::var("AMATERASU_TEST_MARKER") {
                std::fs::write(marker, b"main-started").unwrap();
            }
            std::thread::sleep(Duration::from_secs(30));
        }
        "capture-env-cwd" => {
            let marker = std::env::var("AMATERASU_TEST_MARKER").unwrap();
            let value = std::env::var("AMATERASU_TEST_VALUE").unwrap();
            let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
            std::fs::write(marker, format!("{value}|{}", cwd.display())).unwrap();
        }
        "proton-environment" => {
            let marker = std::env::var("AMATERASU_TEST_MARKER").unwrap();
            let value = |name| std::env::var(name).unwrap_or_else(|_| "<unset>".into());
            std::fs::write(
                marker,
                format!(
                    "{}|{}",
                    value("PROTON_KEEP_SONY_AUDIO_ENDPOINT_VISIBLE"),
                    value("PROTON_SONY_WINDOWS_DEVICE_NAMES")
                ),
            )
            .unwrap();
        }
        "tree" | "tree_exit" | "branch" | "leaf" => tree_child(),
        "trace" | "trace_fail" | "trace_sleep" | "trace_fail_once" => trace_child(),
        #[cfg(unix)]
        "graceful" => graceful_child(),
        #[cfg(windows)]
        "window-graceful" => window_graceful_child(),
        "cwd" => {
            let actual = std::env::current_dir().unwrap().canonicalize().unwrap();
            let expected = std::path::PathBuf::from(std::env::var("AMATERASU_TEST_CWD").unwrap())
                .canonicalize()
                .unwrap();
            assert_eq!(actual, expected);
        }
        _ => {}
    }
}

#[test]
fn steam_launch_without_an_install_directory_is_unknown_and_never_kills_the_launcher() {
    let marker = std::env::temp_dir().join(format!(
        "amaterasu-steam-detached-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut request = launch("steam-untracked", "steam-marker");
    request["steam_id"] = json!("42");
    request["steam_install_dir"] = json!(
        std::env::temp_dir()
            .join("missing-amaterasu-steam-game")
            .to_string_lossy()
    );
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);

    let mut manager = ProcessManager::default();
    let launched = manager.handle("alice", request);
    assert_eq!(launched["ok"], true, "{launched}");
    let process_id = launched["process"]["process_id"].as_u64().unwrap();
    assert_eq!(launched["process"]["steam_game_liveness"], "unknown");

    let stopped = manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    assert_eq!(stopped["ok"], true, "{stopped}");
    assert_eq!(stopped["process"]["state"], "cancelled");

    for _ in 0..30 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(marker.exists(), "Steam launcher child was signaled on stop");
    let _ = std::fs::remove_file(marker);
}

#[cfg(target_os = "linux")]
#[test]
fn steam_install_tree_association_tracks_and_stops_only_the_new_game_process() {
    let install_directory = std::env::temp_dir().join(format!(
        "amaterasu-steam-game-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&install_directory).unwrap();
    let executable = install_directory.join("amaterasu-steam-game-probe");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let game_directory = install_directory.join("game");
    std::fs::create_dir(&game_directory).unwrap();
    let mut request = launch("steam-tracked", "sleep");
    request["executable"] = json!(executable);
    request["steam_id"] = json!("42");
    request["steam_install_dir"] = json!(install_directory);
    request["cwd"] = json!(game_directory);

    let mut manager = ProcessManager::default();
    let launched = manager.handle("alice", request);
    assert_eq!(launched["ok"], true, "{launched}");
    let process_id = launched["process"]["process_id"].as_u64().unwrap();

    // The spawned child can still be between fork and exec/chdir when the
    // first status request scans /proc. Association is deliberately based on
    // observed install-tree evidence, so wait for that evidence to appear.
    let association_deadline = Instant::now() + Duration::from_secs(5);
    let associated = loop {
        let response = manager.handle("alice", json!({"action":"status","process_id":process_id}));
        if response["process"]["steam_game_liveness"] == "associated"
            || Instant::now() >= association_deadline
        {
            break response;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        associated["process"]["steam_game_liveness"], "associated",
        "{associated}"
    );

    let stopping = manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    assert_eq!(stopping["process"]["state"], "stopping", "{stopping}");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut stopped = stopping;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        stopped = manager.handle("alice", json!({"action":"status","process_id":process_id}));
        if stopped["process"]["state"] == "cancelled" {
            break;
        }
    }
    assert_eq!(stopped["process"]["state"], "cancelled", "{stopped}");
    let _ = std::fs::remove_dir_all(install_directory);
}

#[cfg(windows)]
#[test]
fn steam_install_tree_association_stops_only_the_verified_game_process() {
    let install_directory = std::env::temp_dir().join(format!(
        "amaterasu-steam-game-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&install_directory).unwrap();
    let game_executable = install_directory.join("steam-test-game.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &game_executable).unwrap();

    let mut request = launch("steam-windows-tracked", "sleep");
    request["executable"] = json!(game_executable);
    request["cwd"] = json!(install_directory);
    request["steam_id"] = json!("42");
    request["steam_install_dir"] = json!(install_directory);

    let mut manager = ProcessManager::default();
    let launched = manager.handle("alice", request);
    assert_eq!(launched["ok"], true, "{launched}");
    let process_id = launched["process"]["process_id"].as_u64().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut associated = status(&mut manager, "alice", process_id);
    while Instant::now() < deadline && associated["process"]["steam_game_liveness"] != "associated"
    {
        std::thread::sleep(Duration::from_millis(50));
        associated = status(&mut manager, "alice", process_id);
    }
    assert_eq!(
        associated["process"]["steam_game_liveness"], "associated",
        "{associated}"
    );

    let mut unrelated = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::controlled_child", "--ignored"])
        .env("AMATERASU_TEST_CHILD", "sleep")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let stopping = manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    assert_eq!(stopping["process"]["state"], "stopping", "{stopping}");
    let cancelled = wait_for_state(&mut manager, "alice", process_id, "cancelled");
    assert_eq!(cancelled["process"]["state"], "cancelled", "{cancelled}");
    assert!(
        unrelated.try_wait().unwrap().is_none(),
        "Steam stop signaled an unrelated same-user process"
    );
    let _ = unrelated.kill();
    let _ = unrelated.wait();
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(install_directory).unwrap();
}

#[cfg(windows)]
#[test]
fn steam_install_tree_stop_posts_wm_close_only_to_the_verified_game_window() {
    let install_directory = std::env::temp_dir().join(format!(
        "amaterasu-steam-window-game-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&install_directory).unwrap();
    let game_executable = install_directory.join("steam-window-test.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &game_executable).unwrap();
    let closed_marker = install_directory.join("window-closed");
    let ready_marker = install_directory.join("window-ready");

    let mut request = launch("steam-windows-graceful", "window-graceful");
    request["executable"] = json!(game_executable);
    request["cwd"] = json!(install_directory);
    request["steam_id"] = json!("42");
    request["steam_install_dir"] = json!(install_directory);
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(closed_marker);
    request["environment"]["AMATERASU_TEST_READY"] = json!(ready_marker);

    let mut manager = ProcessManager::default();
    let launched = manager.handle("alice", request);
    assert_eq!(launched["ok"], true, "{launched}");
    let process_id = launched["process"]["process_id"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut associated = status(&mut manager, "alice", process_id);
    while Instant::now() < deadline
        && (associated["process"]["steam_game_liveness"] != "associated" || !ready_marker.exists())
    {
        std::thread::sleep(Duration::from_millis(50));
        associated = status(&mut manager, "alice", process_id);
    }
    assert_eq!(
        associated["process"]["steam_game_liveness"], "associated",
        "{associated}"
    );
    assert!(
        ready_marker.exists(),
        "game window did not reach its message loop"
    );

    let stopping = manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    assert_eq!(stopping["process"]["state"], "stopping", "{stopping}");
    let cancelled = wait_for_state(&mut manager, "alice", process_id, "cancelled");
    assert_eq!(cancelled["process"]["state"], "cancelled", "{cancelled}");
    assert_eq!(std::fs::read(&closed_marker).unwrap(), b"closed");
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(install_directory).unwrap();
}

fn trace_child() {
    use std::io::Write;

    if let Ok(expected) = std::env::var("AMATERASU_TEST_CWD") {
        let actual = std::env::current_dir().unwrap().canonicalize().unwrap();
        let expected = std::path::PathBuf::from(expected).canonicalize().unwrap();
        assert_eq!(actual, expected);
    }

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
    writeln!(std::io::stdout(), "OUT-{event}").unwrap();
    writeln!(std::io::stderr(), "ERR-{event}").unwrap();

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

fn set_lifecycle_working_directory(command: &mut Value, cwd: &std::path::Path) {
    command["cwd"] = json!(cwd);
    command["environment"]["AMATERASU_TEST_CWD"] = json!(cwd);
}

fn trace_events(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|contents| contents.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn quote_raw_command_argument(argument: &str) -> String {
    #[cfg(windows)]
    {
        format!("\"{argument}\"")
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", argument.replace('\'', "'\\''"))
    }
}

fn raw_test_command(argument: &str) -> String {
    format!(
        "{} {argument}",
        quote_raw_command_argument("$(AMATERASU_RAW_PREPARATION_TEST_EXE)")
    )
}

fn output_command(label: &str) -> Value {
    json!({
        "executable":std::env::current_exe().unwrap(),
        "args":["--exact","tests::controlled_child","--ignored","--",format!("--application-output={label}")],
        "timeout_ms":5_000,
    })
}

fn wait_for_output(path: &std::path::Path, marker: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let output = std::fs::read_to_string(path).unwrap_or_default();
        if output.contains(marker) {
            return output;
        }
        assert!(
            Instant::now() < deadline,
            "command output marker missing: {marker}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn application_output_appends_all_command_phases_and_detached_overrides_take_precedence() {
    let directory = test_directory("shared-application-output");
    let log = directory.join("application.log");
    let child_out = directory.join("child.out");
    let child_err = directory.join("child.err");
    std::fs::write(&log, "existing-output\n").unwrap();
    let mut request = launch("shared-application-output", "application-output-main");
    request["output"] = json!(log);
    request["exit_timeout_seconds"] = json!(0);
    request["environment"]["AMATERASU_RAW_PREPARATION_TEST_EXE"] =
        json!(std::env::current_exe().unwrap());
    request["preparation"] = json!([
        {"run":output_command("prepare-structured"),"undo":output_command("undo-structured")},
        {"do":raw_test_command("--exact tests::controlled_child --ignored -- --application-output=prepare-raw"),
         "undo":raw_test_command("--exact tests::controlled_child --ignored -- --application-output=undo-raw")},
    ]);
    let detached = format!(
        "{} --exact tests::controlled_child --ignored",
        quote_raw_command_argument(std::env::current_exe().unwrap().to_str().unwrap())
    );
    request["detached"] = json!([
        {"command":detached,"environment":{"AMATERASU_TEST_CHILD":"detached-output","AMATERASU_TEST_VALUE":"default-detached"}},
        {"command":detached,"environment":{"AMATERASU_TEST_CHILD":"detached-output","AMATERASU_TEST_VALUE":"overridden"},"stdout":child_out,"stderr":child_err},
        {"command":detached,"environment":{"AMATERASU_TEST_CHILD":"detached-output","AMATERASU_TEST_VALUE":"partial"},"stdout":child_out},
    ]);

    let mut manager = ProcessManager::default();
    let started = manager.handle("alice", request.clone());
    let id = started["process"]["process_id"].as_u64().unwrap();
    wait_for_state(&mut manager, "alice", id, "running");
    wait_for_output(&log, "ERR-main");
    wait_for_output(&log, "ERR-default-detached");
    wait_for_output(&log, "ERR-partial");
    wait_for_output(&child_out, "OUT-overridden");
    wait_for_output(&child_out, "OUT-partial");
    wait_for_output(&child_err, "ERR-overridden");
    manager.handle("alice", json!({"action":"terminate","process_id":id}));
    let complete = wait_for_state(&mut manager, "alice", id, "cancelled");
    let output = std::fs::read_to_string(&log).unwrap();
    assert!(output.starts_with("existing-output\n"));
    for label in [
        "prepare-structured",
        "prepare-raw",
        "default-detached",
        "main",
        "undo-raw",
        "undo-structured",
    ] {
        assert!(output.contains(&format!("OUT-{label}")), "{output}");
        assert!(output.contains(&format!("ERR-{label}")), "{output}");
    }
    assert!(output.find("OUT-prepare-structured") < output.find("OUT-prepare-raw"));
    assert!(output.find("OUT-prepare-raw") < output.find("OUT-main"));
    assert!(output.find("OUT-main") < output.find("OUT-undo-raw"));
    assert!(output.find("OUT-undo-raw") < output.find("OUT-undo-structured"));
    assert!(!output.contains("OUT-overridden") && !output.contains("ERR-overridden"));
    assert!(!output.contains("OUT-partial"));
    assert!(
        !serde_json::to_string(&complete)
            .unwrap()
            .contains("prepare-structured")
    );
    assert_eq!(
        manager.handle("alice", request)["process"]["state"],
        "cancelled"
    );
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        output,
        "replay appended output"
    );
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn output_open_failure_is_retained_before_any_command_has_effects() {
    let directory = test_directory("failed-application-output");
    let missing_log = directory.join("missing-parent").join("output.log");
    let marker = directory.join("ran");
    let mut request = launch("failed-application-output", "exit");
    request["output"] = json!(missing_log);
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);
    request["preparation"] = json!([{"run":lifecycle_command(&marker,"prepare","trace")}]);
    request["detached"] = json!([format!(
        "{} --exact tests::controlled_child --ignored",
        quote_raw_command_argument(std::env::current_exe().unwrap().to_str().unwrap())
    )]);
    let mut manager = ProcessManager::default();
    let failed = manager.handle("alice", request.clone());
    assert_eq!(failed["process"]["state"], "launch_failed", "{failed}");
    assert_eq!(failed["process"]["pid"], Value::Null);
    assert_eq!(
        failed["process"]["preparation"]["failure"]["kind"],
        "output_open_failed"
    );
    assert!(!marker.exists());
    std::fs::create_dir(missing_log.parent().unwrap()).unwrap();
    assert_eq!(manager.handle("alice", request), failed);
    assert!(!missing_log.exists(), "retry reopened failed output");

    let prepare = json!({"action":"prepare","operation_id":"failed-prepare-output","output":directory,"preparation":[{"run":lifecycle_command(&marker,"prepare-only","trace")}]});
    let failed_lease = manager.handle("alice", prepare.clone());
    assert_eq!(failed_lease["preparation"]["state"], "preparation_failed");
    assert_eq!(
        failed_lease["preparation"]["preparation"]["failure"]["kind"],
        "output_open_failed"
    );
    assert_eq!(manager.handle("alice", prepare), failed_lease);
    assert!(!marker.exists());
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn prepare_output_keeps_the_opened_file_through_failed_undo_and_retry() {
    let directory = test_directory("prepare-output-owned-file");
    let log = directory.join("output.log");
    let rotated = directory.join("opened-output.log");
    let trace = directory.join("events");
    let request = json!({
        "action":"prepare",
        "operation_id":"prepare-output-owned-file",
        "output":log,
        "preparation":[{
            "run":lifecycle_command(&trace,"prepare","trace"),
            "undo":lifecycle_command(&trace,"undo-attempt","trace_fail_once"),
        }],
    });
    let mut manager = ProcessManager::default();
    assert_eq!(manager.handle("alice", request.clone())["ok"], true);
    wait_for_preparation_state(&mut manager, "alice", "prepare-output-owned-file", "ready");
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("OUT-prepare")
    );
    std::fs::rename(&log, &rotated).unwrap();
    std::fs::write(&log, "replacement-file\n").unwrap();
    manager.handle(
        "alice",
        json!({"action":"preparation_stop","operation_id":"prepare-output-owned-file"}),
    );
    let failed = wait_for_preparation_state(
        &mut manager,
        "alice",
        "prepare-output-owned-file",
        "cleanup_failed",
    );
    assert_eq!(failed["preparation"]["cleanup"]["remaining"], 1);
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "replacement-file\n");
    assert_eq!(
        std::fs::read_to_string(&rotated)
            .unwrap()
            .matches("OUT-undo-attempt")
            .count(),
        1
    );
    manager.handle(
        "alice",
        json!({"action":"preparation_retry","operation_id":"prepare-output-owned-file"}),
    );
    let complete = wait_for_preparation_state(
        &mut manager,
        "alice",
        "prepare-output-owned-file",
        "complete",
    );
    assert_eq!(complete["preparation"]["cleanup"]["remaining"], 0);
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "replacement-file\n");
    assert_eq!(
        std::fs::read_to_string(&rotated)
            .unwrap()
            .matches("OUT-undo-attempt")
            .count(),
        2
    );
    assert_eq!(manager.handle("alice", request), complete);
    #[cfg(target_os = "linux")]
    {
        let retained = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .flatten()
            .any(|entry| {
                std::fs::read_link(entry.path()).ok().as_deref() == Some(rotated.as_path())
            });
        assert!(
            !retained,
            "completed preparation retained its output handle"
        );
    }
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn null_output_normalizes_without_redirecting_or_changing_retry_identity() {
    let mut manager = ProcessManager::default();
    let request = launch("null-application-output", "exit");
    let started = manager.handle("alice", request.clone());
    let id = started["process"]["process_id"].as_u64().unwrap();
    let exited = wait_for_exit(&mut manager, id);
    for output in [Value::Null, json!(""), json!("null")] {
        let mut retry = request.clone();
        retry["output"] = output;
        assert_eq!(manager.handle("alice", retry), exited);
    }
    let directory = test_directory("invalid-application-output");
    for output in [
        json!("relative.log"),
        json!(format!("{}\n", directory.display())),
        json!(directory.join("x".repeat(1025))),
    ] {
        let mut invalid = launch("invalid-application-output", "exit");
        invalid["output"] = output;
        assert_eq!(manager.handle("alice", invalid)["code"], "invalid_launch");
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn output_only_provider_lease_opens_once_and_closes_after_stop() {
    let directory = test_directory("output-only-provider-lease");
    let log = directory.join("provider.log");
    let request =
        json!({"action":"prepare","operation_id":"output-only-provider-lease","output":log});
    let mut manager = ProcessManager::default();
    assert_eq!(manager.handle("alice", request.clone())["ok"], true);
    assert!(log.exists());
    let ready =
        wait_for_preparation_state(&mut manager, "alice", "output-only-provider-lease", "ready");
    assert_eq!(ready["preparation"]["preparation"]["completed"], 0);
    assert!(
        manager.handle("alice", json!({"action":"list"}))["processes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    manager.handle(
        "alice",
        json!({"action":"preparation_stop","operation_id":"output-only-provider-lease"}),
    );
    let complete = wait_for_preparation_state(
        &mut manager,
        "alice",
        "output-only-provider-lease",
        "complete",
    );
    assert_eq!(manager.handle("alice", request), complete);
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
    #[cfg(target_os = "linux")]
    assert!(
        !std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .flatten()
            .any(|entry| std::fs::read_link(entry.path()).ok().as_deref() == Some(log.as_path())),
        "completed output-only lease retained a file handle"
    );
    std::fs::remove_dir_all(directory).unwrap();
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

#[cfg(target_os = "linux")]
fn proton_environment_for_launch(
    manager: &mut ProcessManager,
    operation_id: &str,
    marker: &std::path::Path,
    steam_id: Option<&str>,
    compatibility: Option<bool>,
    overrides: &[(&str, &str)],
) -> String {
    let mut request = launch(operation_id, "proton-environment");
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker.to_string_lossy().into_owned());
    if let Some(steam_id) = steam_id {
        request["steam_id"] = json!(steam_id);
    }
    if let Some(compatibility) = compatibility {
        request["proton_dualsense_compatibility"] = json!(compatibility);
    }
    for (name, value) in overrides {
        request["environment"][*name] = json!(value);
    }

    let started = manager.handle("alice", request);
    assert_eq!(started["ok"], true, "{started}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        manager.reap_exited().unwrap();
        if let Ok(value) = std::fs::read_to_string(marker)
            && value.contains('|')
        {
            return value;
        }
        assert!(Instant::now() < deadline, "child did not write {marker:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn proton_dualsense_environment_is_opt_in_for_steam_launches_and_preserves_overrides() {
    let directory = test_directory("proton-dualsense-environment");
    let mut manager = ProcessManager::default();

    assert_eq!(
        proton_environment_for_launch(
            &mut manager,
            "proton-defaults",
            &directory.join("defaults"),
            Some("42"),
            Some(true),
            &[],
        ),
        "1|1"
    );
    assert_eq!(
        proton_environment_for_launch(
            &mut manager,
            "proton-overrides",
            &directory.join("overrides"),
            Some("43"),
            Some(true),
            &[
                ("PROTON_KEEP_SONY_AUDIO_ENDPOINT_VISIBLE", "0"),
                ("PROTON_SONY_WINDOWS_DEVICE_NAMES", "custom"),
            ],
        ),
        "0|custom"
    );
    assert_eq!(
        proton_environment_for_launch(
            &mut manager,
            "proton-empty-override",
            &directory.join("empty-override"),
            Some("44"),
            Some(true),
            &[("PROTON_KEEP_SONY_AUDIO_ENDPOINT_VISIBLE", "")],
        ),
        "|1"
    );
    assert_eq!(
        proton_environment_for_launch(
            &mut manager,
            "proton-legacy-default",
            &directory.join("legacy-default"),
            Some("45"),
            None,
            &[],
        ),
        "<unset>|<unset>"
    );
    assert_eq!(
        proton_environment_for_launch(
            &mut manager,
            "proton-non-steam",
            &directory.join("non-steam"),
            None,
            Some(true),
            &[],
        ),
        "<unset>|<unset>"
    );

    std::thread::sleep(Duration::from_millis(100));
    manager.reap_exited().unwrap();
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
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
fn detached_environment_retries_reuse_resolution_and_reject_changed_launch_fields() {
    const HOST_VARIABLE: &str = "AMATERASU_TEST_DETACHED_RETRY_HOST_VALUE";

    let mut manager = ProcessManager::default();
    let mut request = launch("detached-host-retry", "sleep");
    request["detached"] = json!([format!(
        "amaterasu-missing-detached-command \"$({HOST_VARIABLE})\""
    )]);

    let first = manager.handle_with_environment_lookup("alice", request.clone(), &|name| {
        (name == HOST_VARIABLE).then(|| "first host value".to_owned())
    });
    assert_eq!(first["process"]["state"], "running", "{first}");

    let replay = manager.handle_with_environment_lookup("alice", request.clone(), &|name| {
        (name == HOST_VARIABLE).then(|| "changed host value".to_owned())
    });
    assert_eq!(replay, first);

    let mut typed_equivalent = request.clone();
    typed_equivalent["detached"] = json!([{"command":request["detached"][0]}]);
    assert_eq!(
        manager.handle_with_environment_lookup("alice", typed_equivalent, &|name| {
            (name == HOST_VARIABLE).then(|| "newer host value".to_owned())
        }),
        first
    );

    let mut changed_detached = request.clone();
    changed_detached["detached"] = json!([format!(
        "amaterasu-missing-detached-command \"$({HOST_VARIABLE})\" --changed"
    )]);
    assert_eq!(
        manager.handle_with_environment_lookup("alice", changed_detached, &|name| {
            (name == HOST_VARIABLE).then(|| "third host value".to_owned())
        })["code"],
        "operation_conflict"
    );

    let mut changed_arguments = request;
    changed_arguments["args"] = json!(["changed launch argument"]);
    assert_eq!(
        manager.handle_with_environment_lookup("alice", changed_arguments, &|_| None)["code"],
        "operation_conflict"
    );

    manager.shutdown().unwrap();
}

#[test]
fn detached_retry_rejects_changed_override_even_when_effective_environment_matches() {
    let mut manager = ProcessManager::default();
    let mut request = launch("detached-redundant-override", "sleep");
    request["environment"]["SAME_VALUE"] = json!("same");
    request["detached"] = json!([{
        "command":"amaterasu-missing-detached-command",
        "environment":{"SAME_VALUE":"same"}
    }]);
    let started = manager.handle("alice", request.clone());
    assert_eq!(started["process"]["state"], "running", "{started}");
    assert_eq!(manager.handle("alice", request.clone()), started);

    request["detached"][0]["environment"] = json!({});
    assert_eq!(
        manager.handle("alice", request)["code"],
        "operation_conflict"
    );
    manager.shutdown().unwrap();
}

#[test]
fn typed_detached_elevation_is_validated_and_part_of_retry_identity() {
    let mut manager = ProcessManager::default();
    let mut invalid = launch("detached-elevation-invalid", "sleep");
    invalid["detached"] = json!([{
        "command":"amaterasu-missing-detached-command",
        "elevated":"yes"
    }]);
    assert_eq!(manager.handle("alice", invalid)["code"], "invalid_request");
    assert_eq!(
        manager.handle("alice", json!({"action":"list"}))["processes"],
        json!([])
    );

    let mut request = launch("detached-elevation", "sleep");
    request["elevated"] = json!(true);
    request["detached"] = json!([{
        "command":"amaterasu-missing-detached-command",
        "elevated":false
    }]);
    let started = manager.handle("alice", request.clone());
    assert_eq!(started["process"]["state"], "running", "{started}");
    assert_eq!(manager.handle("alice", request.clone()), started);
    request["detached"][0]["elevated"] = json!(true);
    assert_eq!(
        manager.handle("alice", request.clone())["code"],
        "operation_conflict"
    );
    request["detached"][0]
        .as_object_mut()
        .unwrap()
        .remove("elevated");
    assert_eq!(
        manager.handle("alice", request)["code"],
        "operation_conflict"
    );
    manager.shutdown().unwrap();
}

#[test]
fn auto_detach_retains_a_quick_successful_launcher_until_explicit_stop() {
    let directory = test_directory("auto-detach");
    let marker = directory.join("launcher-exited");
    let mut manager = ProcessManager::default();
    let mut request = launch("auto-detach", "exit");
    request.as_object_mut().unwrap().remove("auto_detach");
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);

    let started = manager.handle("alice", request.clone());
    let id = started["process"]["process_id"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "launcher did not exit quickly");
        manager.reap_exited().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(20));
    manager.reap_exited().unwrap();
    let retained = status(&mut manager, "alice", id);
    assert_eq!(retained["process"]["state"], "running", "{retained}");

    manager.handle("alice", json!({"action":"terminate","process_id":id}));
    let cancelled = wait_for_state(&mut manager, "alice", id, "cancelled");
    assert_eq!(manager.handle("alice", request), cancelled);
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn auto_detach_does_not_retain_a_failed_launcher() {
    let mut manager = ProcessManager::default();
    let mut request = launch("auto-detach-failure", "exit-failure");
    request["auto_detach"] = json!(true);
    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();

    let exited = wait_for_state(&mut manager, "alice", id, "exited");
    assert_eq!(exited["process"]["success"], false);
    manager.shutdown().unwrap();
}

fn detached_command_for_current_test() -> String {
    detached_command_for_current_test_with_arguments(&["--detached-child"])
}

fn detached_command_for_current_test_with_arguments(arguments: &[&str]) -> String {
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut command_arguments = vec![
        quote_raw_command_argument(&executable),
        quote_raw_command_argument("--exact"),
        quote_raw_command_argument("tests::controlled_child"),
        quote_raw_command_argument("--ignored"),
        quote_raw_command_argument("--"),
    ];
    command_arguments.extend(
        arguments
            .iter()
            .map(|argument| quote_raw_command_argument(argument)),
    );
    command_arguments.join(" ")
}

#[cfg(windows)]
unsafe extern "system" fn graceful_window_procedure(
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DestroyWindow, PostQuitMessage, WM_CLOSE, WM_DESTROY,
    };

    match message {
        WM_CLOSE => {
            if let (Ok(marker), Ok(ready)) = (
                std::env::var("AMATERASU_TEST_MARKER"),
                std::env::var("AMATERASU_TEST_READY"),
            ) {
                let _ = std::fs::write(marker, b"closed");
                let _ = std::fs::write(ready, b"closed");
            }
            // SAFETY: the window is the live HWND delivered by its message loop.
            unsafe { DestroyWindow(window) };
            0
        }
        WM_DESTROY => {
            // SAFETY: posting quit from the window's owning thread ends its loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => {
            // SAFETY: unhandled messages retain the system default window behavior.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}

#[cfg(windows)]
fn window_graceful_child() {
    use std::ptr;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CW_USEDEFAULT, CreateWindowExW, DispatchMessageW, GetMessageW, MSG, RegisterClassW,
        TranslateMessage, UnregisterClassW, WNDCLASSW, WS_OVERLAPPEDWINDOW,
    };

    let class_name = "AmaterasuGracefulTestWindow\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    assert!(!instance.is_null());
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(graceful_window_procedure),
        hInstance: instance,
        lpszClassName: class_name.as_ptr(),
        ..Default::default()
    };
    // SAFETY: the class and callback remain alive until the message loop exits.
    assert_ne!(unsafe { RegisterClassW(&window_class) }, 0);
    // SAFETY: class name is NUL terminated, instance is valid, and no parent or
    // creation data is needed for this isolated test window.
    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        )
    };
    assert!(!window.is_null());
    let ready = std::env::var("AMATERASU_TEST_READY").unwrap();
    std::fs::write(ready, b"ready").unwrap();

    let mut message = MSG::default();
    loop {
        // SAFETY: `message` is writable and this thread owns the window queue.
        let status = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        assert!(status >= 0);
        if status == 0 {
            break;
        }
        // SAFETY: `message` was populated by GetMessageW above.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // SAFETY: all windows of this class were destroyed before the queue quit.
    unsafe { UnregisterClassW(class_name.as_ptr(), instance) };
}

#[test]
fn detached_commands_outlive_the_primary_application_and_are_reaped() {
    let directory = test_directory("detached-commands");
    let marker = directory.join("detached-finished");
    let mut manager = ProcessManager::default();
    let mut request = launch("detached-commands", "sleep");
    request["detached"] = json!([{"command":detached_command_for_current_test(),"environment":{}}]);
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);

    let started = manager.handle("alice", request);
    assert_eq!(started["process"]["state"], "running", "{started}");
    let id = started["process"]["process_id"].as_u64().unwrap();
    let stopped = manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(stopped["process"]["state"], "stopping", "{stopped}");
    assert_eq!(
        wait_for_state(&mut manager, "alice", id, "cancelled")["process"]["state"],
        "cancelled"
    );

    let deadline = Instant::now() + Duration::from_secs(3);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "detached command was killed with its app"
        );
        manager.reap_exited().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    manager.reap_exited().unwrap();
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn detached_commands_expand_configured_values_before_platform_parsing() {
    const CONFIGURED_VARIABLE: &str = "AMATERASU_TEST_DETACHED_EXPANSION_VALUE";
    const MISSING_VARIABLE: &str = "AMATERASU_TEST_DETACHED_EXPANSION_MISSING_20260925";

    let directory = test_directory("detached-environment-expansion");
    let marker = directory.join("expanded-arguments");
    assert!(std::env::var_os(MISSING_VARIABLE).is_none());

    let detached_arguments = [
        "--detached-arguments".to_owned(),
        format!("configured=$({CONFIGURED_VARIABLE})"),
        format!("escaped=$$({CONFIGURED_VARIABLE})"),
        format!("missing=$({MISSING_VARIABLE})"),
    ];
    let detached_argument_refs = detached_arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();

    let mut manager = ProcessManager::default();
    let mut request = launch("detached-environment-expansion", "sleep");
    request["detached"] = json!([detached_command_for_current_test_with_arguments(
        &detached_argument_refs
    )]);
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);
    request["environment"][CONFIGURED_VARIABLE] = json!("Desk Mode");

    let started = manager.handle("alice", request);
    assert_eq!(started["process"]["state"], "running", "{started}");

    let escaped_argument = format!("escaped=$({CONFIGURED_VARIABLE})");
    let expected_arguments = [
        "configured=Desk Mode",
        escaped_argument.as_str(),
        "missing=",
    ]
    .join("\n");
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(&marker).ok().as_deref() != Some(expected_arguments.as_str()) {
        assert!(
            Instant::now() < deadline,
            "detached child did not record expanded argv: {:?}",
            std::fs::read_to_string(&marker)
        );
        manager.reap_exited().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }

    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        expected_arguments
    );
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn typed_detached_commands_isolate_environment_and_cwd_from_primary_and_each_other() {
    let directory = test_directory("typed-detached");
    let primary_cwd = directory.join("primary");
    let first_cwd = directory.join("first");
    let second_cwd = directory.join("second");
    for cwd in [&primary_cwd, &first_cwd, &second_cwd] {
        std::fs::create_dir(cwd).unwrap();
    }
    let primary_marker = directory.join("primary-marker");
    let first_marker = directory.join("first-marker");
    let second_marker = directory.join("second-marker");
    let command = detached_command_for_current_test_with_arguments(&[]);

    let mut request = launch("typed-detached", "capture-env-cwd");
    request["cwd"] = json!(primary_cwd);
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(primary_marker);
    request["environment"]["AMATERASU_TEST_VALUE"] = json!("primary");
    request["detached"] = json!([
        {"command":command,"cwd":first_cwd,"environment":{"AMATERASU_TEST_MARKER":first_marker,"AMATERASU_TEST_VALUE":"first"}},
        {"command":command,"working_directory":second_cwd,"environment":{"AMATERASU_TEST_MARKER":second_marker,"AMATERASU_TEST_VALUE":"second"}}
    ]);
    let mut manager = ProcessManager::default();
    let started = manager.handle("alice", request.clone());
    assert_eq!(started["ok"], true, "{started}");
    let id = started["process"]["process_id"].as_u64().unwrap();
    assert_eq!(wait_for_exit(&mut manager, id)["process"]["success"], true);

    let deadline = Instant::now() + Duration::from_secs(5);
    while !first_marker.exists() || !second_marker.exists() {
        assert!(
            Instant::now() < deadline,
            "detached commands did not write markers"
        );
        manager.reap_exited().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    for (marker, value, cwd) in [
        (&primary_marker, "primary", &primary_cwd),
        (&first_marker, "first", &first_cwd),
        (&second_marker, "second", &second_cwd),
    ] {
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            format!("{value}|{}", cwd.canonicalize().unwrap().display())
        );
    }

    let mut alias_retry = request.clone();
    let alias_cwd = alias_retry["detached"][1]["working_directory"].clone();
    alias_retry["detached"][1]
        .as_object_mut()
        .unwrap()
        .remove("working_directory");
    alias_retry["detached"][1]["cwd"] = alias_cwd;
    assert_eq!(
        manager.handle("alice", alias_retry)["process"]["process_id"],
        id
    );
    let mut changed = request;
    changed["detached"][0]["environment"]["AMATERASU_TEST_VALUE"] = json!("changed");
    assert_eq!(
        manager.handle("alice", changed)["code"],
        "operation_conflict"
    );
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn detached_directory_selection_preserves_legacy_inheritance_and_independent_targets() {
    let directory = test_directory("detached-directory-policy");
    let primary_directory = directory.join("primary");
    let helper_directory = directory.join("helper");
    let configured_directory = directory.join("configured");
    let child_directory = directory.join("child");
    for selected in [
        &primary_directory,
        &helper_directory,
        &configured_directory,
        &child_directory,
    ] {
        std::fs::create_dir(selected).unwrap();
    }
    let test_executable = std::env::current_exe().unwrap();
    let executable_name = test_executable.file_name().unwrap();
    let primary_executable = primary_directory.join(executable_name);
    let helper_executable = helper_directory.join(executable_name);
    std::fs::copy(&test_executable, &primary_executable).unwrap();
    std::fs::copy(&test_executable, &helper_executable).unwrap();
    let command = detached_command_for_current_test_with_arguments(&[]).replacen(
        &quote_raw_command_argument(test_executable.to_str().unwrap()),
        &quote_raw_command_argument(helper_executable.to_str().unwrap()),
        1,
    );

    for (name, policy, child_override, expected_directory) in [
        ("legacy", None, None, &primary_directory),
        ("inferred", Some(Value::Null), None, &helper_directory),
        (
            "configured",
            Some(json!(configured_directory)),
            None,
            &configured_directory,
        ),
        (
            "child",
            Some(json!(configured_directory)),
            Some(&child_directory),
            &child_directory,
        ),
    ] {
        let marker = directory.join(format!("{name}-marker"));
        let mut request = launch(&format!("detached-directory-{name}"), "exit");
        request["executable"] = json!(primary_executable);
        request["cwd"] = json!(primary_directory);
        if let Some(policy) = policy {
            request["detached_cwd"] = policy;
        }
        let mut detached = json!({
            "command":command,
            "environment":{
                "AMATERASU_TEST_CHILD":"capture-env-cwd",
                "AMATERASU_TEST_VALUE":name,
                "AMATERASU_TEST_MARKER":marker,
            },
        });
        if let Some(child_override) = child_override {
            detached["cwd"] = json!(child_override);
        }
        request["detached"] = json!([detached]);

        let mut manager = ProcessManager::default();
        let started = manager.handle("alice", request.clone());
        assert_eq!(started["ok"], true, "{started}");
        let process_id = started["process"]["process_id"].as_u64().unwrap();
        assert_eq!(
            wait_for_exit(&mut manager, process_id)["process"]["success"],
            true
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "{name} child did not write its directory"
            );
            manager.reap_exited().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            format!(
                "{name}|{}",
                expected_directory.canonicalize().unwrap().display()
            )
        );
        request["detached_cwd"] = if request["detached_cwd"].is_null() {
            json!(configured_directory)
        } else {
            Value::Null
        };
        assert_eq!(
            manager.handle("alice", request)["code"],
            "operation_conflict"
        );
        manager.shutdown().unwrap();
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn detached_commands_resolve_merged_path_and_keep_missing_targets_best_effort() {
    let directory = test_directory("detached-effective-path");
    let executable_directory = directory.join("bin");
    std::fs::create_dir(&executable_directory).unwrap();
    let executable_name = if cfg!(windows) {
        "path-probe.exe"
    } else {
        "path-probe"
    };
    let executable = executable_directory.join(executable_name);
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    let marker = directory.join("detached-record");
    let original = detached_command_for_current_test_with_arguments(&[]);
    let original_executable =
        quote_raw_command_argument(std::env::current_exe().unwrap().to_str().unwrap());
    let command = original.replacen(
        &original_executable,
        &quote_raw_command_argument(executable_name),
        1,
    );
    let mut request = launch("detached-effective-path", "sleep");
    request["cwd"] = json!(directory);
    request["environment"]["PATH"] = json!("missing-launch-path");
    request["detached"] = json!([
        "amaterasu-missing-detached-probe-92831",
        {
            "command":command,
            "environment":{
                "PATH":"bin",
                "AMATERASU_TEST_CHILD":"capture-env-cwd",
                "AMATERASU_TEST_MARKER":marker,
                "AMATERASU_TEST_VALUE":"selected-command-path",
            },
        },
    ]);
    let mut manager = ProcessManager::default();
    let started = manager.handle("alice", request);
    assert_eq!(started["process"]["state"], "running", "{started}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        manager.reap_exited().unwrap();
        assert!(
            Instant::now() < deadline,
            "configured-PATH detached target did not run"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        format!(
            "selected-command-path|{}",
            directory.canonicalize().unwrap().display()
        ),
    );
    let id = started["process"]["process_id"].as_u64().unwrap();
    manager.handle("alice", json!({"action":"terminate","process_id":id}));
    wait_for_state(&mut manager, "alice", id, "cancelled");
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn typed_detached_output_is_appended_to_independent_child_files() {
    let directory = test_directory("typed-detached-output");
    let first_out = directory.join("first-stdout");
    let first_err = directory.join("first-stderr");
    let second_out = directory.join("second-stdout");
    let second_err = directory.join("second-stderr");
    std::fs::write(&first_out, "existing\n").unwrap();
    let command = detached_command_for_current_test_with_arguments(&[]);
    let mut request = launch("typed-detached-output", "exit");
    request["detached"] = json!([
        {"command":command,"environment":{"AMATERASU_TEST_CHILD":"detached-output","AMATERASU_TEST_VALUE":"first"},"stdout":first_out,"stderr":first_err},
        {"command":command,"environment":{"AMATERASU_TEST_CHILD":"detached-output","AMATERASU_TEST_VALUE":"second"},"stdout":second_out,"stderr":second_err}
    ]);
    let mut manager = ProcessManager::default();
    let started = manager.handle("alice", request.clone());
    assert_eq!(started["ok"], true, "{started}");
    let id = started["process"]["process_id"].as_u64().unwrap();
    assert_eq!(wait_for_exit(&mut manager, id)["process"]["success"], true);
    let deadline = Instant::now() + Duration::from_secs(5);
    while [
        (&first_out, "OUT-first"),
        (&first_err, "ERR-first"),
        (&second_out, "OUT-second"),
        (&second_err, "ERR-second"),
    ]
    .iter()
    .any(|(path, marker)| {
        !std::fs::read_to_string(path)
            .unwrap_or_default()
            .contains(marker)
    }) {
        assert!(Instant::now() < deadline, "detached output markers missing");
        manager.reap_exited().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    for (stdout, stderr, value) in [
        (&first_out, &first_err, "first"),
        (&second_out, &second_err, "second"),
    ] {
        let out = std::fs::read_to_string(stdout).unwrap();
        let err = std::fs::read_to_string(stderr).unwrap();
        assert!(out.contains(&format!("OUT-{value}")), "{out}");
        assert!(err.contains(&format!("ERR-{value}")), "{err}");
        assert!(!out.contains(if value == "first" {
            "OUT-second"
        } else {
            "OUT-first"
        }));
        assert!(!err.contains(if value == "first" {
            "ERR-second"
        } else {
            "ERR-first"
        }));
    }
    assert!(
        std::fs::read_to_string(&first_out)
            .unwrap()
            .starts_with("existing\n")
    );
    assert_eq!(
        manager.handle("alice", request.clone())["process"]["process_id"],
        id
    );
    request["detached"][0]["stdout"] = json!(directory.join("changed-stdout"));
    assert_eq!(
        manager.handle("alice", request)["code"],
        "operation_conflict"
    );
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn invalid_typed_detached_override_rejects_before_any_process_is_spawned() {
    let directory = test_directory("invalid-typed-detached");
    let marker = directory.join("primary-marker");
    let command = detached_command_for_current_test_with_arguments(&[]);
    let mut manager = ProcessManager::default();
    let mut request = launch("invalid-typed-detached", "capture-env-cwd");
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);
    request["environment"]["AMATERASU_TEST_VALUE"] = json!("primary");
    request["detached"] = json!([{"command":command,"environment":{"BAD=KEY":"invalid"}}]);
    assert_eq!(
        manager.handle("alice", request.clone())["code"],
        "invalid_launch"
    );
    assert!(!marker.exists());
    request["detached"][0]["environment"] = json!({});
    request["detached"][0]["cwd"] = json!("relative/path");
    assert_eq!(
        manager.handle("alice", request.clone())["code"],
        "invalid_launch"
    );
    request["detached"] = json!([
        detached_command_for_current_test(),
        {"command":command,"stdout":"relative/output"}
    ]);
    assert_eq!(manager.handle("alice", request)["code"], "invalid_launch");
    std::thread::sleep(Duration::from_millis(150));
    assert!(!marker.exists());
    let mut request = launch("invalid-detached-output", "capture-env-cwd");
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);
    request["environment"]["AMATERASU_TEST_VALUE"] = json!("primary");
    request["detached"] = json!([{"command":command,"stdout":"relative/output"}]);
    assert_eq!(manager.handle("alice", request)["code"], "invalid_launch");
    assert!(!marker.exists());
    assert_eq!(
        manager.handle("alice", json!({"action":"list"}))["processes"],
        json!([])
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn merged_detached_environment_limit_rejects_before_primary_spawn() {
    let directory = test_directory("merged-detached-environment");
    let marker = directory.join("primary-marker");
    let mut request = launch("merged-detached-environment", "capture-env-cwd");
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);
    request["environment"]["AMATERASU_TEST_VALUE"] = json!("primary");
    for index in 0..30 {
        request["environment"][format!("FILLER_{index}")] = json!("x");
    }
    request["detached"] = json!([{
        "command": detached_command_for_current_test_with_arguments(&[]),
        "environment": {"EXTRA_KEY":"x"}
    }]);
    let mut manager = ProcessManager::default();
    assert_eq!(manager.handle("alice", request)["code"], "invalid_launch");
    assert!(!marker.exists());
    assert_eq!(
        manager.handle("alice", json!({"action":"list"}))["processes"],
        json!([])
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn wait_all_keeps_the_app_running_for_live_process_group_members() {
    let directory = test_directory("wait-all");
    let mut manager = ProcessManager::default();
    let mut request = launch("wait-all", "tree_exit");
    request["environment"]["AMATERASU_TEST_TREE"] = json!(directory);
    request["wait_all"] = json!(true);
    request["exit_timeout_seconds"] = json!(0);
    request["auto_detach"] = json!(false);

    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.join("leaf").exists() {
        assert!(Instant::now() < deadline, "process group did not start");
        manager.reap_exited().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    manager.reap_exited().unwrap();
    let retained = status(&mut manager, "alice", id);
    assert_eq!(retained["process"]["state"], "running", "{retained}");

    manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(
        wait_for_state(&mut manager, "alice", id, "cancelled")["process"]["state"],
        "cancelled"
    );
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(unix)]
fn per_app_exit_timeout_controls_the_graceful_process_group_window() {
    let directory = test_directory("exit-timeout");
    let mut manager = ProcessManager::default();
    let mut request = launch("exit-timeout", "tree");
    request["environment"]["AMATERASU_TEST_TREE"] = json!(directory);
    request["wait_all"] = json!(false);
    request["exit_timeout_seconds"] = json!(1);
    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.join("leaf").exists() {
        assert!(Instant::now() < deadline, "process group did not start");
        manager.reap_exited().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }

    let termination_started = Instant::now();
    manager.handle("alice", json!({"action":"terminate","process_id":id}));
    std::thread::sleep(Duration::from_millis(100));
    manager.reap_exited().unwrap();
    assert_eq!(
        status(&mut manager, "alice", id)["process"]["state"],
        "stopping"
    );
    assert_eq!(
        wait_for_state(&mut manager, "alice", id, "cancelled")["process"]["state"],
        "cancelled"
    );
    assert!(termination_started.elapsed() >= Duration::from_secs(1));
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(windows)]
fn per_app_exit_timeout_requests_wm_close_before_terminating_the_job() {
    let directory = test_directory("windows-exit-timeout");
    let ready = directory.join("window-ready");
    let marker = directory.join("window-closed");
    let mut manager = ProcessManager::default();
    let mut request = launch("windows-exit-timeout", "window-graceful");
    request["environment"]["AMATERASU_TEST_READY"] = json!(ready);
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);
    request["exit_timeout_seconds"] = json!(5);
    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "window child did not become ready"
        );
        manager.reap_exited().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    let stopping = manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(stopping["process"]["state"], "stopping", "{stopping}");
    assert_eq!(
        wait_for_state(&mut manager, "alice", id, "cancelled")["process"]["state"],
        "cancelled"
    );
    assert_eq!(std::fs::read(marker).unwrap(), b"closed");
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
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
fn omitted_working_directory_uses_main_executable_directory() {
    let directory = test_directory("default-main-cwd");
    let source_executable = std::env::current_exe().unwrap();
    let executable = directory.join(source_executable.file_name().unwrap());
    std::fs::copy(source_executable, &executable).unwrap();

    let mut manager = ProcessManager::default();
    let mut request = launch("default-main-cwd", "cwd");
    request["executable"] = json!(executable);
    request["environment"]["AMATERASU_TEST_CWD"] = json!(directory);
    let started = manager.handle("alice", request);
    assert_eq!(started["process"]["state"], "running", "{started}");
    let id = started["process"]["process_id"].as_u64().unwrap();
    assert_eq!(wait_for_exit(&mut manager, id)["process"]["success"], true);
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
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
#[cfg(windows)]
fn failed_job_assignment_never_runs_the_suspended_application() {
    let marker = test_directory("failed-job-assignment").join("ran");
    let mut request = launch("failed-job-assignment", "exit");
    request["environment"]["AMATERASU_TEST_MARKER"] = json!(marker);
    let mut manager = ProcessManager::default();

    crate::containment::fail_next_job_assignment();
    crate::containment::fail_next_failed_spawn_termination();
    let failed = manager.handle("alice", request);
    assert_eq!(failed["process"]["state"], "cleanup_failed", "{failed}");
    assert_eq!(failed["process"]["outcome"], "launch_failed");
    let process_id = failed["process"]["process_id"].as_u64().unwrap();
    assert!(!marker.exists(), "application ran before job assignment");

    let stopping = manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    assert_eq!(stopping["process"]["state"], "stopping", "{stopping}");
    let still_owned = status(&mut manager, "alice", process_id);
    assert_eq!(
        still_owned["process"]["state"], "cleanup_failed",
        "{still_owned}"
    );
    assert!(
        !marker.exists(),
        "unassigned child resumed during failed cleanup"
    );

    let retry = manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    assert_eq!(retry["process"]["state"], "stopping", "{retry}");
    assert_eq!(
        wait_for_state(&mut manager, "alice", process_id, "launch_failed")["process"]["state"],
        "launch_failed"
    );
    assert!(!marker.exists(), "unassigned child ran during retry");

    let recovered = manager.handle("alice", launch("after-job-assignment", "exit"));
    assert_eq!(recovered["process"]["state"], "running", "{recovered}");
    let recovered_id = recovered["process"]["process_id"].as_u64().unwrap();
    assert_eq!(
        wait_for_exit(&mut manager, recovered_id)["process"]["success"],
        true
    );
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(marker.parent().unwrap()).unwrap();
}

#[test]
#[cfg(windows)]
fn failed_preparation_spawn_retains_the_suspended_child_for_retry() {
    let marker = test_directory("failed-preparation-assignment").join("ran");
    let mut request = launch("failed-preparation-assignment", "exit");
    request["preparation"] = json!([{
        "run": {
            "executable": std::env::current_exe().unwrap(),
            "args": ["--exact", "tests::controlled_child", "--ignored"],
            "environment": {
                "AMATERASU_TEST_CHILD": "exit",
                "AMATERASU_TEST_MARKER": marker,
            },
            "timeout_ms": 1000,
        }
    }]);
    let mut manager = ProcessManager::default();
    let launched = manager.handle("alice", request);
    let process_id = launched["process"]["process_id"].as_u64().unwrap();

    crate::containment::fail_next_job_assignment();
    crate::containment::fail_next_failed_spawn_termination();
    let failed = wait_for_state(&mut manager, "alice", process_id, "cleanup_failed");
    assert_eq!(failed["process"]["outcome"], "launch_failed", "{failed}");
    assert!(
        failed["process"]["preparation"]["last_command"].is_null(),
        "an owned suspended child has not settled: {failed}"
    );
    assert!(!marker.exists());

    let retry = manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    assert_eq!(retry["process"]["state"], "stopping", "{retry}");
    let settled = wait_for_state(&mut manager, "alice", process_id, "launch_failed");
    assert_eq!(settled["process"]["cleanup"]["state"], "complete");
    assert_eq!(
        settled["process"]["preparation"]["last_command"]["kind"],
        "spawn_failed"
    );
    assert!(!marker.exists());
    manager.shutdown().unwrap();
    std::fs::remove_dir_all(marker.parent().unwrap()).unwrap();
}

#[test]
#[cfg(windows)]
fn shutdown_recovers_after_a_transient_job_process_query_failure() {
    let mut manager = ProcessManager::default();
    let launched = manager.handle("alice", launch("transient-job-query", "sleep"));
    assert_eq!(launched["process"]["state"], "running", "{launched}");
    let process_id = launched["process"]["process_id"].as_u64().unwrap();

    crate::containment::fail_next_job_process_query();
    assert_eq!(manager.shutdown(), Ok(()));
    let settled = status(&mut manager, "alice", process_id);
    assert_eq!(settled["process"]["state"], "cancelled", "{settled}");

    let recovered = manager.handle("alice", launch("after-job-query", "exit"));
    assert_eq!(recovered["process"]["state"], "running", "{recovered}");
    let recovered_id = recovered["process"]["process_id"].as_u64().unwrap();
    assert_eq!(
        wait_for_exit(&mut manager, recovered_id)["process"]["success"],
        true
    );
    manager.shutdown().unwrap();
}

#[test]
#[cfg(windows)]
fn failed_job_process_query_keeps_the_child_owned_for_stop_retry() {
    let mut manager = ProcessManager::default();
    let launched = manager.handle("alice", launch("job-query-retry", "sleep"));
    assert_eq!(launched["process"]["state"], "running", "{launched}");
    let process_id = launched["process"]["process_id"].as_u64().unwrap();

    crate::containment::fail_next_job_process_query();
    let failed = manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    assert_eq!(failed["code"], "process_kill_failed", "{failed}");
    let retained = status(&mut manager, "alice", process_id);
    assert_eq!(retained["process"]["state"], "running", "{retained}");

    let retry = manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    assert_eq!(retry["process"]["state"], "stopping", "{retry}");
    assert_eq!(
        wait_for_state(&mut manager, "alice", process_id, "cancelled")["process"]["state"],
        "cancelled"
    );
    manager.shutdown().unwrap();
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
    let undo_directory = test_directory("ordered-lifecycle-undo");
    let trace = directory.join("events");
    let mut manager = ProcessManager::default();
    let mut request = launch("prepared", "sleep");
    let mut prepare_one = lifecycle_command(&trace, "prepare-one", "trace");
    let mut undo_one = lifecycle_command(&trace, "undo-one", "trace");
    let mut prepare_two = lifecycle_command(&trace, "prepare-two", "trace");
    let mut undo_two = lifecycle_command(&trace, "undo-two", "trace");
    set_lifecycle_working_directory(&mut prepare_one, &directory);
    set_lifecycle_working_directory(&mut prepare_two, &directory);
    set_lifecycle_working_directory(&mut undo_one, &undo_directory);
    set_lifecycle_working_directory(&mut undo_two, &undo_directory);
    request["preparation"] = json!([
        {
            "run":prepare_one,
            "undo":undo_one,
        },
        {
            "run":prepare_two,
            "undo":undo_two,
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
    assert_eq!(
        running["process"]["preparation"]["last_command"],
        json!({"step":1,"success":true,"exit_code":0})
    );
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
        cancelled["process"]["cleanup"]["last_command"],
        json!({"step":0,"success":true,"exit_code":0})
    );
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
    std::fs::remove_dir_all(undo_directory).unwrap();
}

#[test]
fn raw_preparation_commands_execute_and_undo_through_the_existing_lifecycle() {
    let directory = test_directory("raw-preparation-cwd");
    let mut manager = ProcessManager::default();
    let mut request = launch("raw-preparation", "sleep");
    request["cwd"] = json!(directory);
    request["preparation_cwd"] = json!(directory);
    request["environment"]["AMATERASU_RAW_PREPARATION_TEST_EXE"] =
        json!(std::env::current_exe().unwrap().to_string_lossy());
    request["preparation"] = json!([{
        "do":raw_test_command("--exact tests::controlled_child --ignored -- --raw-preparation-marker"),
        "undo":raw_test_command("--exact tests::controlled_child --ignored -- --raw-preparation-marker"),
        "elevated":false,
    }]);

    let started = manager.handle("alice", request);
    assert_eq!(started["process"]["state"], "preparing", "{started}");
    let process_id = started["process"]["process_id"].as_u64().unwrap();
    let running = wait_for_state(&mut manager, "alice", process_id, "running");
    let marker = directory.join("raw-preparation-marker");
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "raw\n");
    assert_eq!(running["process"]["preparation"]["completed"], 1);
    assert_eq!(running["process"]["cleanup"]["remaining"], 1);

    manager.handle(
        "alice",
        json!({"action":"terminate","process_id":process_id}),
    );
    let cancelled = wait_for_state(&mut manager, "alice", process_id, "cancelled");
    assert_eq!(cancelled["process"]["cleanup"]["state"], "complete");
    assert_eq!(cancelled["process"]["cleanup"]["remaining"], 0);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "raw\nraw\n");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn malformed_later_raw_preparation_rejects_before_any_step_runs() {
    let directory = test_directory("invalid-raw-preparation");
    let trace = directory.join("events");
    let steps = json!([
        {"run": lifecycle_command(&trace, "first-step", "trace")},
        {"do": "tool $(UNCLOSED"}
    ]);
    let mut manager = ProcessManager::default();
    let mut launch_request = launch("invalid-raw-launch", "sleep");
    launch_request["preparation"] = steps.clone();

    assert_eq!(
        manager.handle("alice", launch_request)["code"],
        "invalid_launch"
    );
    assert_eq!(
        manager.handle(
            "alice",
            json!({
                "action": "prepare",
                "operation_id": "invalid-raw-lease",
                "preparation": steps
            })
        )["code"],
        "invalid_preparation"
    );
    assert_eq!(
        manager.handle("alice", json!({"action": "list"}))["processes"],
        json!([])
    );
    assert!(!trace.exists(), "earlier preparation ran before validation");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn prepare_only_lease_is_idempotent_and_undoes_in_reverse_without_launching_a_process() {
    let directory = test_directory("prepare-only-lease");
    let trace = directory.join("events");
    let steps = json!([
        {
            "run":lifecycle_command(&trace, "global-one", "trace"),
            "undo":lifecycle_command(&trace, "undo-one", "trace"),
        },
        {
            "run":lifecycle_command(&trace, "global-two", "trace"),
            "undo":lifecycle_command(&trace, "undo-two", "trace"),
        },
    ]);
    let request = json!({
        "action":"prepare",
        "operation_id":"playnite-preparation",
        "preparation":steps,
    });
    let mut manager = ProcessManager::default();
    let started = manager.handle("alice", request.clone());
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["preparation"]["state"], "preparing");
    assert_eq!(
        manager.handle("alice", json!({"action":"list"}))["processes"],
        json!([]),
        "prepare-only created an application process record"
    );

    let ready = wait_for_preparation_state(&mut manager, "alice", "playnite-preparation", "ready");
    assert_eq!(ready["preparation"]["preparation"]["completed"], 2);
    assert_eq!(trace_events(&trace), ["global-one", "global-two"]);
    assert_eq!(manager.handle("alice", request.clone()), ready);

    let mut changed_plan = request.clone();
    changed_plan["preparation"][0]["run"] = lifecycle_command(&trace, "different-step", "trace");
    assert_eq!(
        manager.handle("alice", changed_plan)["code"],
        "operation_conflict"
    );

    let stopping = manager.handle(
        "alice",
        json!({"action":"preparation_stop","operation_id":"playnite-preparation"}),
    );
    assert_eq!(stopping["preparation"]["state"], "cleaning", "{stopping}");
    let complete =
        wait_for_preparation_state(&mut manager, "alice", "playnite-preparation", "complete");
    assert_eq!(complete["preparation"]["cleanup"]["state"], "complete");
    assert_eq!(
        trace_events(&trace),
        ["global-one", "global-two", "undo-two", "undo-one"]
    );
    assert_eq!(manager.handle("alice", request.clone()), complete);
    assert_eq!(
        manager.handle(
            "alice",
            json!({"action":"preparation_stop","operation_id":"playnite-preparation"}),
        ),
        complete
    );
    assert_eq!(
        manager.handle(
            "bob",
            json!({"action":"preparation_status","operation_id":"playnite-preparation"}),
        )["code"],
        "preparation_not_found"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn prepare_exit_policy_defaults_is_bounded_and_changes_conflict() {
    let mut manager = ProcessManager::default();
    let request = json!({
        "action":"prepare",
        "operation_id":"prepare-default-exit-policy",
        "preparation":[{}],
    });
    assert_eq!(manager.handle("alice", request.clone())["ok"], true);
    let mut explicit_default = request.clone();
    explicit_default["exit_timeout_seconds"] = json!(10);
    assert_eq!(manager.handle("alice", explicit_default)["ok"], true);
    let mut conflict = request;
    conflict["exit_timeout_seconds"] = json!(15);
    assert_eq!(
        manager.handle("alice", conflict)["code"],
        "operation_conflict"
    );
    for timeout in [0, 300] {
        assert_eq!(
            manager.handle(
                "alice",
                json!({
                    "action":"prepare",
                    "operation_id":format!("prepare-exit-policy-{timeout}"),
                    "preparation":[{}],
                    "exit_timeout_seconds":timeout,
                })
            )["ok"],
            true
        );
    }
    assert_eq!(
        manager.handle(
            "alice",
            json!({
                "action":"prepare",
                "operation_id":"prepare-exit-policy-oversized",
                "preparation":[{}],
                "exit_timeout_seconds":301,
            })
        )["code"],
        "invalid_preparation"
    );
    manager.shutdown().unwrap();
}

#[test]
fn prepare_only_failure_rolls_back_started_steps_and_cleanup_can_be_retried() {
    let directory = test_directory("prepare-only-failure");
    let trace = directory.join("events");
    let mut manager = ProcessManager::default();
    let response = manager.handle(
        "alice",
        json!({
            "action":"prepare",
            "operation_id":"playnite-prepare-failure",
            "preparation":[
                {
                    "run":lifecycle_command(&trace, "prepare-one", "trace"),
                    "undo":lifecycle_command(&trace, "undo-one", "trace"),
                },
                {
                    "run":lifecycle_command(&trace, "prepare-fail", "trace_fail"),
                    "undo":lifecycle_command(&trace, "undo-failed-step", "trace"),
                },
            ],
        }),
    );
    assert_eq!(response["ok"], true, "{response}");
    let failed = wait_for_preparation_state(
        &mut manager,
        "alice",
        "playnite-prepare-failure",
        "preparation_failed",
    );
    assert_eq!(failed["preparation"]["preparation"]["failure"]["step"], 1);
    assert_eq!(
        trace_events(&trace),
        [
            "prepare-one",
            "prepare-fail",
            "undo-failed-step",
            "undo-one"
        ]
    );

    let retry_directory = test_directory("prepare-only-retry");
    let retry_trace = retry_directory.join("events");
    let mut retry = ProcessManager::default();
    let started = retry.handle(
        "alice",
        json!({
            "action":"prepare",
            "operation_id":"playnite-prepare-retry",
            "preparation":[{
                "run":lifecycle_command(&retry_trace, "prepare", "trace"),
                "undo":lifecycle_command(&retry_trace, "undo-attempt", "trace_fail_once"),
            }],
        }),
    );
    assert_eq!(started["ok"], true, "{started}");
    wait_for_preparation_state(&mut retry, "alice", "playnite-prepare-retry", "ready");
    let stopping = retry.handle(
        "alice",
        json!({"action":"preparation_stop","operation_id":"playnite-prepare-retry"}),
    );
    assert_eq!(stopping["preparation"]["state"], "cleaning", "{stopping}");
    let failed_undo = wait_for_preparation_state(
        &mut retry,
        "alice",
        "playnite-prepare-retry",
        "cleanup_failed",
    );
    assert_eq!(
        failed_undo["preparation"]["state"], "cleanup_failed",
        "{failed_undo}"
    );
    assert_eq!(
        failed_undo["preparation"]["cleanup"]["failures"][0]["phase"],
        "undo"
    );
    let retried = retry.handle(
        "alice",
        json!({"action":"preparation_retry","operation_id":"playnite-prepare-retry"}),
    );
    assert_eq!(retried["preparation"]["state"], "cleaning", "{retried}");
    let complete =
        wait_for_preparation_state(&mut retry, "alice", "playnite-prepare-retry", "complete");
    assert_eq!(complete["preparation"]["cleanup"]["state"], "complete");
    assert_eq!(
        trace_events(&retry_trace),
        ["prepare", "undo-attempt", "undo-attempt"]
    );
    std::fs::remove_dir_all(directory).unwrap();
    std::fs::remove_dir_all(retry_directory).unwrap();
}

#[test]
fn shutdown_does_not_retry_a_failed_preparation_cleanup() {
    let directory = test_directory("shutdown-preparation-failure");
    let trace = directory.join("events");
    let mut manager = ProcessManager::default();
    let started = manager.handle(
        "alice",
        json!({
            "action":"prepare",
            "operation_id":"shutdown-preparation-failure",
            "preparation":[{
                "run":lifecycle_command(&trace, "prepare", "trace"),
                "undo":lifecycle_command(&trace, "undo", "trace_fail_once"),
            }],
        }),
    );
    assert_eq!(started["ok"], true, "{started}");
    wait_for_preparation_state(
        &mut manager,
        "alice",
        "shutdown-preparation-failure",
        "ready",
    );

    assert_eq!(manager.shutdown(), Err("process_cleanup_failed"));
    assert_eq!(trace_events(&trace), ["prepare", "undo"]);
    let failed = manager.handle(
        "alice",
        json!({"action":"preparation_status","operation_id":"shutdown-preparation-failure"}),
    );
    assert_eq!(failed["preparation"]["state"], "cleanup_failed");

    let retried = manager.handle(
        "alice",
        json!({"action":"preparation_retry","operation_id":"shutdown-preparation-failure"}),
    );
    assert_eq!(retried["preparation"]["state"], "cleaning");
    wait_for_preparation_state(
        &mut manager,
        "alice",
        "shutdown-preparation-failure",
        "complete",
    );
    assert_eq!(trace_events(&trace), ["prepare", "undo", "undo"]);
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
    assert_eq!(
        failed["process"]["preparation"]["last_command"],
        json!({"step":1,"success":false,"exit_code":3,"kind":"exit_failed"})
    );
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
fn merged_global_preparation_failure_rolls_back_before_app_preparation() {
    let directory = test_directory("merged-global-preparation-failure");
    let trace = directory.join("events");
    let mut manager = ProcessManager::default();
    let mut request = launch("merged-global-preparation-failure", "sleep");
    request["preparation"] = json!([
        {
            "run":lifecycle_command(&trace, "global-prepare-one", "trace"),
            "undo":lifecycle_command(&trace, "global-undo-one", "trace"),
        },
        {
            "run":lifecycle_command(&trace, "global-prepare-fail", "trace_fail"),
            "undo":lifecycle_command(&trace, "global-undo-fail", "trace"),
        },
        {
            "run":lifecycle_command(&trace, "app-prepare-never-runs", "trace"),
            "undo":lifecycle_command(&trace, "app-undo-never-runs", "trace"),
        },
    ]);

    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();
    let failed = wait_for_state(&mut manager, "alice", id, "launch_failed");
    assert_eq!(failed["process"]["preparation"]["completed"], 1);
    assert_eq!(failed["process"]["preparation"]["failure"]["step"], 1);
    assert_eq!(
        trace_events(&trace),
        [
            "global-prepare-one",
            "global-prepare-fail",
            "global-undo-fail",
            "global-undo-one",
        ]
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
        // Keep this below the deliberately 30-second child delay while tolerating parallel test load.
        assert!(
            tick_started.elapsed() < Duration::from_secs(2),
            "a lifecycle tick blocked on a long command for {:?}",
            tick_started.elapsed()
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
    assert_eq!(
        cancelled["process"]["preparation"]["last_command"]["kind"],
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
    request["preparation"] = json!([
        {
            "run":lifecycle_command(&trace, "prepare-one", "trace"),
            "undo":lifecycle_command(&trace, "undo-one", "trace"),
        },
        {
            "run":lifecycle_command(&trace, "prepare-two", "trace"),
            "undo":lifecycle_command(&trace, "undo-two-attempt", "trace_fail_once"),
        },
    ]);

    let started = manager.handle("alice", request);
    let id = started["process"]["process_id"].as_u64().unwrap();
    let running = wait_for_state(&mut manager, "alice", id, "running");
    assert_eq!(running["process"]["preparation"]["completed"], 2);
    manager.handle("alice", json!({"action":"terminate","process_id":id}));
    let failed = wait_for_state(&mut manager, "alice", id, "cleanup_failed");
    assert_eq!(failed["process"]["outcome"], "cancelled");
    assert_eq!(failed["process"]["cleanup"]["state"], "failed");
    assert_eq!(failed["process"]["cleanup"]["remaining"], 1);
    assert_eq!(
        failed["process"]["cleanup"]["failures"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(failed["process"]["cleanup"]["failures"][0]["phase"], "undo");
    assert_eq!(failed["process"]["cleanup"]["failures"][0]["step"], 1);
    assert_eq!(
        failed["process"]["cleanup"]["failures"][0]["kind"],
        "exit_failed"
    );
    assert_eq!(
        trace_events(&trace),
        ["prepare-one", "prepare-two", "undo-two-attempt", "undo-one"]
    );

    for _ in 0..3 {
        assert_eq!(status(&mut manager, "alice", id), failed);
    }
    assert_eq!(
        trace_events(&trace),
        ["prepare-one", "prepare-two", "undo-two-attempt", "undo-one"]
    );

    let retry = manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(retry["process"]["state"], "stopping", "{retry}");
    let cancelled = wait_for_state(&mut manager, "alice", id, "cancelled");
    assert_eq!(cancelled["process"]["cleanup"]["state"], "complete");
    assert_eq!(
        trace_events(&trace),
        [
            "prepare-one",
            "prepare-two",
            "undo-two-attempt",
            "undo-one",
            "undo-two-attempt"
        ]
    );

    manager.handle("alice", json!({"action":"terminate","process_id":id}));
    assert_eq!(
        trace_events(&trace),
        [
            "prepare-one",
            "prepare-two",
            "undo-two-attempt",
            "undo-one",
            "undo-two-attempt"
        ]
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
        request["exit_timeout_seconds"] = json!(0);
        if mode == "tree_exit" {
            request["wait_all"] = json!(false);
        }
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
