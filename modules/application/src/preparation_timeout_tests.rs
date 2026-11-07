use super::*;
use serde_json::json;

/// Keep native test children owned even when a deadline assertion panics.
struct OwnedLifecycle(ApplicationLifecycle);

impl std::ops::Deref for OwnedLifecycle {
    type Target = ApplicationLifecycle;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for OwnedLifecycle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for OwnedLifecycle {
    fn drop(&mut self) {
        let started = Instant::now();
        let logical_start = self
            .0
            .active
            .as_ref()
            .and_then(|active| active.task.stop_requested_at)
            .unwrap_or(started)
            .max(started);
        let deadline = started + Duration::from_secs(3);
        while self.0.has_active_command() && Instant::now() < deadline {
            self.0.abort_active_command();
            self.0.tick(logical_start + started.elapsed());
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn sleeping_command() -> String {
    let executable = std::env::current_exe().unwrap();
    #[cfg(windows)]
    let quoted = format!("\"{}\"", executable.display());
    #[cfg(not(windows))]
    let quoted = shlex::try_quote(executable.to_str().unwrap()).unwrap();
    format!("{quoted} --exact tests::controlled_child --ignored")
}

fn undo_lifecycle(exit_timeout_seconds: u64, step: PreparationStep) -> OwnedLifecycle {
    let mut lifecycle = ApplicationLifecycle::new_with_context(
        vec![step],
        BTreeMap::from([("AMATERASU_TEST_CHILD".into(), "sleep".into())]),
        None,
        exit_timeout_seconds,
    )
    .unwrap();
    lifecycle.pending_undo.push(0);
    lifecycle.phase = Phase::Cleaning;
    lifecycle.cleanup.state = CleanupState::Pending;
    lifecycle.cleanup.remaining = 1;
    OwnedLifecycle(lifecycle)
}

fn settle_cancelled_command(lifecycle: &mut ApplicationLifecycle, now: Instant) {
    lifecycle.abort_active_command();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(3);
    while lifecycle.has_active_command() {
        lifecycle.tick(now + Duration::from_secs(1) + started.elapsed());
        assert!(
            Instant::now() < deadline,
            "test child was not contained and reaped"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn successful_undo_is_not_reexecuted_after_group_observation_failure() {
    let trace = std::env::temp_dir().join(format!(
        "amaterasu-undo-group-observation-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&trace);
    let command: ConfiguredCommand = serde_json::from_value(json!({
        "executable": std::env::current_exe().unwrap(),
        "args": ["--exact", "tests::controlled_child", "--ignored"],
        "environment": {
            "AMATERASU_TEST_CHILD": "trace",
            "AMATERASU_TEST_TRACE": trace,
            "AMATERASU_TEST_EVENT": "undo"
        },
        "timeout_ms": 5000
    }))
    .unwrap();
    let step = PreparationStep::Structured(StructuredPreparationStep {
        run: command.clone(),
        undo: Some(command),
    });
    let mut lifecycle = undo_lifecycle(30, step);
    let start = Instant::now();
    lifecycle.start_next_undo(start);
    lifecycle
        .active
        .as_mut()
        .unwrap()
        .task
        .fail_next_reap_observation = true;
    let deadline = start + Duration::from_secs(5);
    while !lifecycle.cleanup_failed() {
        lifecycle.tick(start + Duration::from_secs(2) + start.elapsed());
        assert!(
            Instant::now() < deadline,
            "expected group observation failure"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(lifecycle.active.as_ref().unwrap().task.observed_exit);
    assert_eq!(std::fs::read_to_string(&trace).unwrap(), "undo\n");

    while !lifecycle.is_cleanup_complete() {
        if lifecycle.cleanup_failed() {
            lifecycle.retry_cleanup();
        }
        lifecycle.tick(start + Duration::from_secs(2) + start.elapsed());
        assert!(Instant::now() < deadline, "cleanup did not complete");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(std::fs::read_to_string(&trace).unwrap(), "undo\n");
    std::fs::remove_file(trace).unwrap();
}

#[cfg(unix)]
#[test]
fn successful_undo_is_not_reexecuted_after_exit_observation_failure() {
    let trace = std::env::temp_dir().join(format!(
        "amaterasu-undo-exit-observation-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&trace);
    let command: ConfiguredCommand = serde_json::from_value(json!({
        "executable": std::env::current_exe().unwrap(),
        "args": ["--exact", "tests::controlled_child", "--ignored"],
        "environment": {
            "AMATERASU_TEST_CHILD": "trace",
            "AMATERASU_TEST_TRACE": trace,
            "AMATERASU_TEST_EVENT": "undo"
        },
        "timeout_ms": 5000
    }))
    .unwrap();
    let step = PreparationStep::Structured(StructuredPreparationStep {
        run: command.clone(),
        undo: Some(command),
    });
    let mut lifecycle = undo_lifecycle(30, step);
    let start = Instant::now();
    let deadline = start + Duration::from_secs(5);
    lifecycle.start_next_undo(start);
    // WNOWAIT checks the real child's exit without consuming the identity that
    // the lifecycle must retain through process-group retirement.
    while !lifecycle
        .active
        .as_mut()
        .unwrap()
        .task
        .child
        .exited()
        .unwrap()
    {
        assert!(Instant::now() < deadline, "undo child did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
    lifecycle
        .active
        .as_mut()
        .unwrap()
        .task
        .fail_next_exit_observation = true;
    lifecycle.tick(start + Duration::from_secs(2));
    assert!(lifecycle.cleanup_failed());
    assert!(!lifecycle.active.as_ref().unwrap().task.observed_exit);
    // Resume at the shutdown force cutoff: an already exited child must be
    // observed before assigning a timeout to the command.
    lifecycle.prepare_shutdown(start + Duration::from_secs(4));
    while !lifecycle.is_cleanup_complete() {
        if lifecycle.cleanup_failed() {
            lifecycle.retry_cleanup();
        }
        lifecycle.tick(start + Duration::from_secs(2) + start.elapsed());
        assert!(Instant::now() < deadline, "cleanup did not complete");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(std::fs::read_to_string(&trace).unwrap(), "undo\n");
    std::fs::remove_file(trace).unwrap();
}

#[cfg(unix)]
#[test]
fn successful_undo_keeps_its_exit_across_shutdown_force_cutoff() {
    let trace = std::env::temp_dir().join(format!(
        "amaterasu-undo-stop-observation-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&trace);
    let command: ConfiguredCommand = serde_json::from_value(json!({
        "executable": std::env::current_exe().unwrap(),
        "args": ["--exact", "tests::controlled_child", "--ignored"],
        "environment": {
            "AMATERASU_TEST_CHILD": "trace",
            "AMATERASU_TEST_TRACE": trace,
            "AMATERASU_TEST_EVENT": "undo"
        },
        "timeout_ms": 5000
    }))
    .unwrap();
    let step = PreparationStep::Structured(StructuredPreparationStep {
        run: command.clone(),
        undo: Some(command),
    });
    let mut lifecycle = undo_lifecycle(30, step);
    let start = Instant::now();
    let deadline = start + Duration::from_secs(5);
    lifecycle.start_next_undo(start);
    lifecycle
        .active
        .as_mut()
        .unwrap()
        .task
        .fail_next_stop_observation = true;
    while !lifecycle.cleanup_failed() {
        lifecycle.tick(start + start.elapsed());
        assert!(
            Instant::now() < deadline,
            "expected stop observation failure"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(lifecycle.active.as_ref().unwrap().task.observed_exit);
    lifecycle.prepare_shutdown(start + Duration::from_secs(4));
    while !lifecycle.is_cleanup_complete() {
        if lifecycle.cleanup_failed() {
            lifecycle.retry_cleanup();
        }
        lifecycle.tick(start + Duration::from_secs(2) + start.elapsed());
        assert!(Instant::now() < deadline, "cleanup did not complete");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(std::fs::read_to_string(&trace).unwrap(), "undo\n");
    std::fs::remove_file(trace).unwrap();
}

#[cfg(unix)]
#[test]
fn observation_failure_keeps_a_running_undo_until_it_finishes() {
    let trace = std::env::temp_dir().join(format!("amaterasu-undo-running-{}", std::process::id()));
    let release = trace.with_extension("release");
    let _ = std::fs::remove_file(&trace);
    let _ = std::fs::remove_file(&release);
    let command: ConfiguredCommand = serde_json::from_value(json!({
        "executable": "/bin/sh",
        "args": ["-c", "printf 'undo\\n' >> \"$1\"; while [ ! -e \"$2\" ]; do /bin/sleep 0.01; done", "test-undo", trace, release],
        "environment": {},
        "timeout_ms": 5000
    })).unwrap();
    let step = PreparationStep::Structured(StructuredPreparationStep {
        run: command.clone(),
        undo: Some(command),
    });
    let mut lifecycle = undo_lifecycle(30, step);
    let start = Instant::now();
    let deadline = start + Duration::from_secs(5);
    lifecycle.start_next_undo(start);
    while !trace.exists() {
        assert!(Instant::now() < deadline, "undo did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
    lifecycle
        .active
        .as_mut()
        .unwrap()
        .task
        .fail_next_exit_observation = true;
    lifecycle.tick(start);
    assert!(lifecycle.cleanup_failed());
    lifecycle.tick(start);
    assert!(lifecycle.active.as_ref().unwrap().task.outcome.is_none());
    std::fs::write(&release, b"continue").unwrap();
    while !lifecycle.is_cleanup_complete() {
        if lifecycle.cleanup_failed() {
            lifecycle.retry_cleanup();
        }
        lifecycle.tick(start + Duration::from_secs(2) + start.elapsed());
        assert!(Instant::now() < deadline, "cleanup did not complete");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(std::fs::read_to_string(&trace).unwrap(), "undo\n");
    std::fs::remove_file(trace).unwrap();
    std::fs::remove_file(release).unwrap();
}

#[test]
fn raw_undo_uses_the_exit_policy_minimum_and_remains_owned_at_timeout() {
    for exit_timeout_seconds in [0, 10, 16, 300] {
        let step = PreparationStep::Raw(RawPreparationStep {
            do_command: String::new(),
            undo: sleeping_command(),
            elevated: false,
        });
        let mut lifecycle = undo_lifecycle(exit_timeout_seconds, step);
        let now = Instant::now();
        lifecycle.start_next_undo(now);
        let timeout = Duration::from_secs(exit_timeout_seconds.max(15));
        assert_eq!(
            lifecycle.active.as_ref().unwrap().task.execution_deadline,
            Some(now + timeout)
        );
        lifecycle.tick(now + timeout - Duration::from_millis(1));
        assert!(lifecycle.has_active_command());
        assert!(lifecycle.active.as_ref().unwrap().task.outcome.is_none());
        lifecycle.tick(now + timeout);
        assert!(
            lifecycle.has_active_command(),
            "timeout released a live child"
        );
        assert!(matches!(
            lifecycle
                .active
                .as_ref()
                .unwrap()
                .task
                .outcome
                .as_ref()
                .unwrap()
                .kind,
            CommandFailureKind::TimedOut
        ));
        settle_cancelled_command(&mut lifecycle, now + timeout);
        assert!(lifecycle.cleanup_failed());
        assert_eq!(lifecycle.pending_undo, [0]);
        assert!(matches!(
            lifecycle.cleanup.last_command.as_ref().unwrap().kind,
            Some(CommandFailureKind::TimedOut)
        ));
    }
}

#[test]
fn structured_undo_keeps_its_timeout_and_raw_run_waits_without_deadline() {
    let run: ConfiguredCommand = serde_json::from_value(json!({
        "executable":std::env::current_exe().unwrap(),
        "args":["--exact","tests::controlled_child","--ignored"],
        "environment":{"AMATERASU_TEST_CHILD":"sleep"},
        "timeout_ms":25,
    }))
    .unwrap();
    let step = PreparationStep::Structured(StructuredPreparationStep {
        run: run.clone(),
        undo: Some(run),
    });
    let mut lifecycle = undo_lifecycle(300, step);
    let now = Instant::now();
    lifecycle.start_next_undo(now);
    assert_eq!(
        lifecycle.active.as_ref().unwrap().task.execution_deadline,
        Some(now + Duration::from_millis(25))
    );
    settle_cancelled_command(&mut lifecycle, now);

    let raw = PreparationStep::Raw(RawPreparationStep {
        do_command: sleeping_command(),
        undo: String::new(),
        elevated: false,
    });
    let mut lifecycle = OwnedLifecycle(
        ApplicationLifecycle::new_with_context(
            vec![raw],
            BTreeMap::from([("AMATERASU_TEST_CHILD".into(), "sleep".into())]),
            None,
            300,
        )
        .unwrap(),
    );
    lifecycle.start_next_preparation(now);
    assert_eq!(
        lifecycle.active.as_ref().unwrap().task.execution_deadline,
        None
    );
    lifecycle.tick(now + Duration::from_secs(11));
    assert!(lifecycle.has_active_command());
    assert!(lifecycle.active.as_ref().unwrap().task.outcome.is_none());
    settle_cancelled_command(&mut lifecycle, now + Duration::from_secs(11));
}

#[test]
fn shutdown_budget_tracks_only_owed_undo_and_covers_the_longest_raw_waits() {
    let step = PreparationStep::Raw(RawPreparationStep {
        do_command: "configured-command".into(),
        undo: "configured-undo".into(),
        elevated: false,
    });
    let mut lifecycle = ApplicationLifecycle::new_with_context(
        vec![step; MAX_PREPARATION_STEPS],
        BTreeMap::new(),
        None,
        300,
    )
    .unwrap();
    let containment_budget = COMMAND_STOP_GRACE + COMMAND_REAP_TIMEOUT;
    assert_eq!(lifecycle.cleanup_budget(), containment_budget);
    lifecycle.pending_undo = (0..MAX_PREPARATION_STEPS).collect();
    assert_eq!(
        lifecycle.cleanup_budget(),
        (Duration::from_secs(300) + containment_budget) * 8 + containment_budget
    );
    lifecycle.complete_undo(7);
    assert_eq!(
        lifecycle.cleanup_budget(),
        (Duration::from_secs(300) + containment_budget) * 7 + containment_budget
    );
}

#[test]
fn announced_deadline_caps_an_active_command_without_starting_cleanup() {
    let step = PreparationStep::Raw(RawPreparationStep {
        do_command: String::new(),
        undo: sleeping_command(),
        elevated: false,
    });
    let mut lifecycle = undo_lifecycle(300, step);
    let now = Instant::now();
    lifecycle.start_next_undo(now);
    let deadline = now + Duration::from_secs(20);
    lifecycle.prepare_shutdown(deadline);
    assert!(lifecycle.phase == Phase::Cleaning);
    assert!(lifecycle.active.as_ref().unwrap().task.outcome.is_none());
    assert_eq!(
        lifecycle.active.as_ref().unwrap().task.stop_requested_at,
        None
    );
    let execution_deadline = deadline - COMMAND_STOP_GRACE - COMMAND_REAP_TIMEOUT;
    assert_eq!(
        lifecycle.active.as_ref().unwrap().task.execution_deadline,
        Some(execution_deadline)
    );
    lifecycle.prepare_shutdown(deadline + Duration::from_secs(100));
    assert_eq!(
        lifecycle.active.as_ref().unwrap().task.execution_deadline,
        Some(execution_deadline)
    );
}

#[test]
fn an_expired_announced_deadline_does_not_start_an_owed_undo() {
    let step = PreparationStep::Raw(RawPreparationStep {
        do_command: String::new(),
        undo: std::env::temp_dir()
            .join("amaterasu-never-start-expired-undo")
            .to_string_lossy()
            .into_owned(),
        elevated: false,
    });
    let mut lifecycle = ApplicationLifecycle::new(vec![step]).unwrap();
    lifecycle.tick(Instant::now());
    let now = Instant::now();
    lifecycle.prepare_shutdown(now);
    lifecycle.begin_cleanup();
    lifecycle.tick(now);
    assert!(lifecycle.cleanup_failed());
    assert_eq!(lifecycle.pending_undo, [0]);
    assert!(!lifecycle.has_active_command());
    assert!(matches!(
        lifecycle.cleanup.failures[0].kind,
        CommandFailureKind::TimedOut
    ));
}

#[test]
fn announcement_and_ordinary_ticks_leave_forward_preparation_untouched_until_cleanup() {
    let mut lifecycle = OwnedLifecycle(
        ApplicationLifecycle::new_with_context(
            vec![PreparationStep::Raw(RawPreparationStep {
                do_command: sleeping_command(),
                undo: String::new(),
                elevated: false,
            })],
            BTreeMap::from([("AMATERASU_TEST_CHILD".into(), "sleep".into())]),
            None,
            300,
        )
        .unwrap(),
    );
    let now = Instant::now();
    lifecycle.start_next_preparation(now);
    lifecycle.prepare_shutdown(now + Duration::from_secs(20));
    lifecycle.tick(now + Duration::from_secs(30));
    assert!(lifecycle.phase == Phase::Preparing);
    let task = &lifecycle.active.as_ref().unwrap().task;
    assert_eq!(task.execution_deadline, None);
    assert_eq!(task.stop_requested_at, None);
    assert!(task.outcome.is_none());
    lifecycle.begin_cleanup();
    let task = &lifecycle.active.as_ref().unwrap().task;
    assert!(task.execution_deadline.unwrap() <= now + Duration::from_secs(20));
    assert!(matches!(
        task.outcome.as_ref().unwrap().kind,
        CommandFailureKind::Cancelled
    ));
}

#[test]
fn later_announcements_preserve_shorter_execution_and_existing_stop_windows() {
    let step = PreparationStep::Raw(RawPreparationStep {
        do_command: String::new(),
        undo: sleeping_command(),
        elevated: false,
    });
    let mut lifecycle = undo_lifecycle(300, step);
    let now = Instant::now();
    lifecycle.start_next_undo(now);
    let original_execution = now + Duration::from_secs(1);
    let original_stop = now - Duration::from_millis(100);
    let task = &mut lifecycle.active.as_mut().unwrap().task;
    task.execution_deadline = Some(original_execution);
    task.stop_requested_at = Some(original_stop);
    task.cancel();
    lifecycle.prepare_shutdown(now + Duration::from_secs(20));
    lifecycle.prepare_shutdown(now + Duration::from_secs(300));
    let task = &lifecycle.active.as_ref().unwrap().task;
    assert_eq!(task.execution_deadline, Some(original_execution));
    assert_eq!(task.stop_requested_at, Some(original_stop));
}

#[test]
fn a_future_undo_uses_the_original_announced_execution_cutoff() {
    let step = PreparationStep::Raw(RawPreparationStep {
        do_command: String::new(),
        undo: sleeping_command(),
        elevated: false,
    });
    let mut lifecycle = undo_lifecycle(300, step);
    let now = Instant::now();
    let deadline = now + Duration::from_secs(20);
    lifecycle.prepare_shutdown(deadline);
    lifecycle.start_next_undo(now + Duration::from_secs(5));
    assert_eq!(
        lifecycle.active.as_ref().unwrap().task.execution_deadline,
        Some(deadline - COMMAND_STOP_GRACE - COMMAND_REAP_TIMEOUT)
    );
}
