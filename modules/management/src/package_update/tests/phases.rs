//! The phase machine: what each request does in each phase, what is refused,
//! and how a worker is cancelled and retired.
use super::*;

/// A host on the first release whose repository also offers the second.
fn host_with_update() -> (Host, SignedSet) {
    let host = Host::running(10, &first_release());
    let update = host.publisher.sign_set(11, &second_release());
    host.publisher.publish(2, &[&host.running, &update]);
    (host, update)
}

/// A service that has checked and downloaded the offered set.
fn ready_service(host: &Host, shell: &mut ScriptedHost) -> TestService {
    let mut service = host.service();
    assert_eq!(
        run(&mut service, shell, Action::Check)["phase"],
        "available"
    );
    assert_eq!(run(&mut service, shell, Action::Download)["phase"], "ready");
    service
}

#[test]
fn an_update_moves_from_check_to_a_request_the_host_holds() {
    let (host, update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "idle"
    );
    assert!(!service.in_progress());

    host.connector.hold();
    assert_eq!(
        ask(&mut service, &mut shell, Action::Check)["phase"],
        "checking"
    );
    assert!(service.in_progress());
    host.connector.release();
    assert_eq!(settle(&mut service, &mut shell)["phase"], "available");
    assert!(!service.in_progress());

    host.connector.hold();
    let downloading = ask(&mut service, &mut shell, Action::Download);
    assert_eq!(downloading["phase"], "downloading");
    assert_eq!(downloading["update"]["digest"], update.digest.as_str());
    assert!(service.in_progress());
    host.connector.release();
    let ready = settle(&mut service, &mut shell);
    assert_eq!(ready["phase"], "ready");
    assert!(!service.in_progress());
    // Asking again changes nothing: the set is already in the cache.
    host.connector.take_requests();
    assert_eq!(
        ask(&mut service, &mut shell, Action::Download)["phase"],
        "ready"
    );
    assert!(host.connector.take_requests().is_empty());

    let waiting = ask(&mut service, &mut shell, Action::Activate);
    assert_eq!(waiting["phase"], "waiting_stream");
    assert!(waiting["error"].is_null());
    assert_eq!(shell.requests.len(), 1);
    assert!(service.in_progress());
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "waiting_stream"
    );

    // The host takes the request once streams have ended.
    shell.report(false, true);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "activating"
    );
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Cancel)),
        "activation_in_progress"
    );
    assert_eq!(shell.withdrawals, 0);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "activating"
    );
}

#[test]
fn a_check_can_be_cancelled_and_started_again() {
    let (host, _update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    host.connector.hold();
    ask(&mut service, &mut shell, Action::Check);
    // The worker is retired before the phase changes.
    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "checking"
    );
    let cancelled = settle(&mut service, &mut shell);
    assert_eq!(cancelled["phase"], "cancelled");
    assert!(cancelled["error"].is_null());
    assert!(cancelled["update"].is_null());
    assert!(cancelled["checked_unix_ms"].is_null());
    assert!(!service.in_progress());

    host.connector.release();
    assert_eq!(
        run(&mut service, &mut shell, Action::Check)["phase"],
        "available"
    );
}

#[test]
fn a_download_can_be_cancelled_and_leaves_nothing_in_the_cache() {
    let (host, update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    run(&mut service, &mut shell, Action::Check);
    host.connector.hold();
    ask(&mut service, &mut shell, Action::Download);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "downloading"
    );
    let cancelled = settle(&mut service, &mut shell);
    assert_eq!(cancelled["phase"], "cancelled");
    assert!(cancelled["error"].is_null());
    // The offer is still known, so the download can be asked for again.
    assert_eq!(cancelled["update"]["digest"], update.digest.as_str());
    assert!(host.cache_entries().is_empty());
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Activate)),
        "not_ready"
    );

    host.connector.release();
    assert_eq!(
        run(&mut service, &mut shell, Action::Download)["phase"],
        "ready"
    );
    assert_eq!(host.cache_entries(), std::slice::from_ref(&update.digest));
}

#[test]
fn a_request_the_host_has_not_started_can_be_withdrawn() {
    let (host, _update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let mut service = ready_service(&host, &mut shell);
    ask(&mut service, &mut shell, Action::Activate);

    let withdrawn = ask(&mut service, &mut shell, Action::Cancel);
    assert_eq!(withdrawn["phase"], "ready");
    assert!(withdrawn["error"].is_null());
    assert_eq!(shell.withdrawals, 1);
    assert!(!service.in_progress());
    // The candidate is still verified in the cache, so it can be asked again.
    assert_eq!(
        ask(&mut service, &mut shell, Action::Activate)["phase"],
        "waiting_stream"
    );
    assert_eq!(shell.requests.len(), 2);

    // A host that has already started refuses the withdrawal.
    shell.withdraws = false;
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Cancel)),
        "activation_in_progress"
    );
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "waiting_stream"
    );
}

#[test]
fn cancel_without_work_changes_nothing() {
    let (host, _update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "idle"
    );
    run(&mut service, &mut shell, Action::Check);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "available"
    );
    run(&mut service, &mut shell, Action::Download);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "ready"
    );
    assert_eq!(shell.withdrawals, 0);
}

#[test]
fn one_piece_of_work_runs_at_a_time() {
    let (host, _update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    let changes = [Action::Check, Action::Download, Action::Activate];

    // Nothing is offered or prepared yet.
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Download)),
        "nothing_to_download"
    );
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Activate)),
        "not_ready"
    );

    host.connector.hold();
    ask(&mut service, &mut shell, Action::Check);
    for action in changes {
        assert_eq!(refusal(&ask(&mut service, &mut shell, action)), "busy");
    }
    host.connector.release();
    settle(&mut service, &mut shell);
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Activate)),
        "not_ready"
    );

    host.connector.hold();
    ask(&mut service, &mut shell, Action::Download);
    for action in changes {
        assert_eq!(refusal(&ask(&mut service, &mut shell, action)), "busy");
    }
    host.connector.release();
    settle(&mut service, &mut shell);

    ask(&mut service, &mut shell, Action::Activate);
    for action in changes {
        assert_eq!(
            refusal(&ask(&mut service, &mut shell, action)),
            "activation_in_progress"
        );
    }
    assert_eq!(shell.requests.len(), 1);
}

#[test]
fn an_update_waits_for_another_install_to_finish() {
    let (host, _update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let mut service = ready_service(&host, &mut shell);
    host.connector.take_requests();
    for action in [Action::Check, Action::Download, Action::Activate] {
        assert_eq!(refusal(&service.request(action, &mut shell, true)), "busy");
    }
    assert!(shell.requests.is_empty());
    assert!(host.connector.take_requests().is_empty());
    // Reading and cancelling never compete with the other install.
    assert_eq!(
        service.request(Action::Status, &mut shell, true)["phase"],
        "ready"
    );
    assert_eq!(
        service.request(Action::Cancel, &mut shell, true)["phase"],
        "ready"
    );
    assert_eq!(
        service.request(Action::Activate, &mut shell, false)["phase"],
        "waiting_stream"
    );
}

#[test]
fn a_host_that_does_not_take_the_request_fails_the_activation() {
    let (host, _update) = host_with_update();
    let mut shell = ScriptedHost::answering(Admission::Unavailable);
    let mut service = ready_service(&host, &mut shell);
    let failed = ask(&mut service, &mut shell, Action::Activate);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "activation_unavailable");
    assert!(!service.in_progress());

    // The host already holds another activation.
    shell.admission = Admission::InProgress;
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Activate)),
        "activation_in_progress"
    );

    // The prepared candidate is kept, so a host that later accepts gets it.
    shell.admission = Admission::Accepted;
    assert_eq!(
        ask(&mut service, &mut shell, Action::Activate)["phase"],
        "waiting_stream"
    );

    // A request that vanished without starting was interrupted.
    shell.report(false, false);
    let interrupted = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(interrupted["phase"], "failed");
    assert_eq!(interrupted["error"], "activation_interrupted");
}

#[test]
fn activation_is_asked_only_for_the_selection_this_module_runs() {
    let (host, _update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let mut service = ready_service(&host, &mut shell);

    // The selection already records a set waiting to be tried.
    let mut selection = host.selection();
    selection.pending = Some("d".repeat(64));
    host.write_selection(&selection);
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Activate)),
        "activation_in_progress"
    );

    // The selection names another active set than the one loaded.
    selection.pending = None;
    selection.active = Some("e".repeat(64));
    host.write_selection(&selection);
    let failed = ask(&mut service, &mut shell, Action::Activate);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "activation_unavailable");

    fs::remove_file(host.root.join("selection.json")).unwrap();
    let failed = ask(&mut service, &mut shell, Action::Activate);
    assert_eq!(failed["error"], "activation_unavailable");
    assert!(shell.requests.is_empty());
}

#[test]
fn a_cancelled_worker_is_retired_before_another_starts() {
    let (host, _update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    host.connector.stall();
    ask(&mut service, &mut shell, Action::Check);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "checking"
    );
    // The worker has not returned, so its phase and its claim both stand.
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "checking"
    );
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Check)),
        "busy"
    );
    assert!(service.in_progress());

    host.connector.release();
    assert_eq!(settle(&mut service, &mut shell)["phase"], "cancelled");
    assert_eq!(
        run(&mut service, &mut shell, Action::Check)["phase"],
        "available"
    );
}

#[test]
fn shutdown_closes_admission_and_waits_for_the_worker() {
    let (host, _update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    host.connector.stall();
    ask(&mut service, &mut shell, Action::Check);

    service.prepare_shutdown();
    for action in [Action::Check, Action::Download, Action::Activate] {
        assert_eq!(
            refusal(&ask(&mut service, &mut shell, action)),
            "shutting_down"
        );
    }
    // Announcing a shutdown does not interrupt work that is running.
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "checking"
    );

    // Quiescing asks the worker to stop and never waits for it.
    let started = Instant::now();
    assert!(!service.quiesce());
    assert!(!service.quiesce());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(service.in_progress());

    host.connector.release();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !service.quiesce() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let stopped = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(stopped["phase"], "cancelled");
    assert_eq!(stopped["error"], "shutting_down");
    assert!(!service.in_progress());
}

#[test]
fn shutdown_stops_a_download_without_publishing_it() {
    let (host, _update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    run(&mut service, &mut shell, Action::Check);
    host.connector.hold();
    ask(&mut service, &mut shell, Action::Download);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !service.quiesce() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(host.cache_entries().is_empty());
    let stopped = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(stopped["phase"], "cancelled");
    assert_eq!(stopped["error"], "shutting_down");
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Download)),
        "shutting_down"
    );
}

#[test]
fn dropping_the_service_retires_its_worker() {
    let (host, _update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    host.connector.hold();
    ask(&mut service, &mut shell, Action::Check);
    // Returns only because the held worker observes cancellation.
    drop(service);
}

#[test]
fn a_set_with_no_recorded_source_has_nothing_to_check() {
    let host = Host::installed(10, &first_release());
    host.publisher.publish(2, &[&host.running]);
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    let status = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(status["ok"], true);
    assert_eq!(status["supported"], true);
    assert_eq!(status["phase"], "idle");
    assert_eq!(status["error"], "no_source");
    assert!(status["source"].is_null());
    assert_eq!(status["running"]["digest"], host.running.digest.as_str());
    assert_eq!(status["running"]["sequence"], "10");

    let checked = ask(&mut service, &mut shell, Action::Check);
    assert_eq!(checked["phase"], "idle");
    assert_eq!(checked["error"], "no_source");
    assert!(!service.in_progress());
    assert!(host.connector.take_requests().is_empty());
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Download)),
        "nothing_to_download"
    );
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Activate)),
        "not_ready"
    );
}

#[test]
fn a_host_that_reports_no_running_set_has_nothing_to_check() {
    let host = Host::running(10, &first_release());
    for environment in [
        Environment::load(
            Some(host.repository_directory()),
            Some(host.root.clone()),
            None,
        ),
        Environment::load(
            Some(host.repository_directory()),
            None,
            Some(host.running.digest.clone()),
        ),
        // The persisted selection never stands in for the loaded digest.
        Environment::load(
            Some(host.repository_directory()),
            Some(host.root.clone()),
            Some("f".repeat(64)),
        ),
        Environment::load(
            None,
            Some(host.root.clone()),
            Some(host.running.digest.clone()),
        ),
    ] {
        let mut service = Service::new(host.connector.clone(), environment);
        let status = ask(&mut service, &mut ScriptedHost::accepting(), Action::Status);
        assert_eq!(status["phase"], "idle");
        assert_eq!(status["error"], "no_source");
        assert!(status["source"].is_null());
    }
    let unknown = Environment::load(None, None, Some("not-a-digest".into()));
    assert!(unknown.running_digest.is_none());
}

#[test]
fn a_failed_activation_is_reported_until_the_next_check() {
    let (host, update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let mut service = ready_service(&host, &mut shell);
    ask(&mut service, &mut shell, Action::Activate);
    drop(service);

    // The host tried the set, rolled back and started the old set again.
    host.write_receipt(&update.digest, "failed");
    let mut restarted = host.service();
    let failed = ask(&mut restarted, &mut shell, Action::Status);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "activation_failed");
    assert_eq!(failed["running"]["digest"], host.running.digest.as_str());
    assert!(failed["update"].is_null());
    assert_eq!(
        refusal(&ask(&mut restarted, &mut shell, Action::Activate)),
        "not_ready"
    );

    let available = run(&mut restarted, &mut shell, Action::Check);
    assert_eq!(available["phase"], "available");
    assert!(available["error"].is_null());
}

#[test]
fn another_flows_receipt_is_not_this_updates_failure() {
    let (host, update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let phase = |host: &Host, shell: &mut ScriptedHost| {
        let mut service = host.service();
        ask(&mut service, shell, Action::Status)["phase"].clone()
    };

    // A set this repository's cache does not hold was another install's.
    host.write_receipt(&update.digest, "failed");
    assert_eq!(phase(&host, &mut shell), "idle");
    drop(ready_service(&host, &mut shell));
    assert_eq!(phase(&host, &mut shell), "failed");

    // An attempt still under way, or one that ended well, is not a failure.
    host.write_receipt(&update.digest, "activating");
    assert_eq!(phase(&host, &mut shell), "idle");
    host.write_receipt(&update.digest, "committed");
    assert_eq!(phase(&host, &mut shell), "idle");
    // Neither is a receipt that names the set that is running.
    host.write_receipt(&host.running.digest, "failed");
    assert_eq!(phase(&host, &mut shell), "idle");
}

#[test]
fn a_host_restarted_into_the_new_set_is_idle_and_then_current() {
    // The repository still lists both releases; the host now runs the second.
    let host = Host::running(11, &second_release());
    let previous = host.publisher.sign_set(10, &first_release());
    host.publisher.publish(2, &[&previous, &host.running]);
    host.write_receipt(&host.running.digest, "committed");
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    let idle = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(idle["phase"], "idle");
    assert!(idle["error"].is_null());
    assert_eq!(idle["running"]["digest"], host.running.digest.as_str());
    assert_eq!(idle["running"]["sequence"], "11");

    let current = run(&mut service, &mut shell, Action::Check);
    assert_eq!(current["phase"], "current");
    assert!(current["update"].is_null());
}
