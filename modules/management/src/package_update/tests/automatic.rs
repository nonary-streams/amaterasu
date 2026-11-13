//! Automatic updates: what the schedule starts in each mode, what holds it
//! back, and what it leaves to the administrator.
//!
//! The host's tick is called with a chosen instant, so no test waits for the
//! schedule. A scheduled check ends at the instant it was ticked with.
use super::*;
use crate::package_update::automatic::{AutomaticMode, CHECK_INTERVAL, FIRST_CHECK_DELAY};
use serde_json::json;

const SECOND: Duration = Duration::from_secs(1);

/// A host on the first release whose repository also offers the second.
fn host_with_update() -> (Host, SignedSet) {
    let host = Host::running(10, &first_release());
    let update = host.publisher.sign_set(11, &second_release());
    host.publisher.publish(2, &[&host.running, &update]);
    (host, update)
}

/// A service whose administrator chose `mode`, and an instant at which its
/// first scheduled check is due.
fn choosing(host: &Host, mode: AutomaticMode) -> (TestService, Instant) {
    let mut service = host.service();
    service.select_automatic(mode);
    (service, Instant::now() + FIRST_CHECK_DELAY)
}

/// Ticks at `now` while another install is active, until the running worker
/// has been retired. Nothing automatic starts meanwhile.
fn retire_beside_another_install(
    service: &mut TestService,
    shell: &mut ScriptedHost,
    now: Instant,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while service.has_worker() {
        assert!(Instant::now() < deadline, "worker did not finish");
        thread::sleep(Duration::from_millis(1));
        service.tick(now, shell, true);
    }
}

/// Ticks through the minutes after `since` and expects nothing to start.
fn nothing_starts(service: &mut TestService, shell: &mut ScriptedHost, since: Instant) {
    for minutes in [0, 1, 30, 359] {
        service.tick(since + Duration::from_secs(minutes * 60), shell, false);
        assert!(!service.has_worker(), "after {minutes} minutes");
    }
}

#[test]
fn nothing_starts_on_its_own_while_automatic_updates_are_off() {
    let (host, _update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    let much_later = Instant::now() + CHECK_INTERVAL * 3;

    service.tick(much_later, &mut shell, false);
    assert!(!service.has_worker());
    let idle = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(idle["phase"], "idle");
    assert_eq!(
        idle["automatic"],
        json!({"mode": "off", "next_check_unix_ms": null})
    );

    // What the administrator asks for stays where it was left.
    assert_eq!(
        run(&mut service, &mut shell, Action::Check)["phase"],
        "available"
    );
    service.tick(much_later, &mut shell, false);
    assert!(!service.has_worker());
    assert_eq!(
        run(&mut service, &mut shell, Action::Download)["phase"],
        "ready"
    );
    service.tick(much_later, &mut shell, false);
    assert!(shell.requests.is_empty());
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "ready"
    );
}

#[test]
fn the_first_check_waits_two_minutes_and_each_later_one_six_hours() {
    let host = Host::running(10, &first_release());
    host.publisher.publish(2, &[&host.running]);
    let mut shell = ScriptedHost::accepting();
    let created = Instant::now();
    let (mut service, first) = choosing(&host, AutomaticMode::Download);

    let announced = ask(&mut service, &mut shell, Action::Status)["automatic"].clone();
    assert_eq!(announced["mode"], "download");
    let expected = unix_milliseconds() + FIRST_CHECK_DELAY.as_millis() as u64;
    let next_check = announced["next_check_unix_ms"].as_u64().unwrap();
    assert!((expected - 60_000..=expected).contains(&next_check));

    service.tick(created + FIRST_CHECK_DELAY - SECOND, &mut shell, false);
    assert!(!service.has_worker());
    service.tick(first, &mut shell, false);
    assert!(service.has_worker());
    let current = tick_to_rest(&mut service, &mut shell, first);
    assert_eq!(current["phase"], "current");
    assert!(current["checked_unix_ms"].is_u64());
    assert_eq!(
        host.connector.take_requests(),
        ["catalog.json", "catalog.sig"]
    );
    let next_check = current["automatic"]["next_check_unix_ms"].as_u64().unwrap();
    let expected = unix_milliseconds() + CHECK_INTERVAL.as_millis() as u64;
    assert!((expected - 60_000..=expected).contains(&next_check));

    // The next one is due six hours after that one finished, not before.
    service.tick(first + CHECK_INTERVAL - SECOND, &mut shell, false);
    assert!(!service.has_worker());
    let second = first + CHECK_INTERVAL;
    service.tick(second, &mut shell, false);
    assert!(service.has_worker());
    tick_to_rest(&mut service, &mut shell, second);
    assert_eq!(
        host.connector.take_requests(),
        ["catalog.json", "catalog.sig"]
    );
    service.tick(second + CHECK_INTERVAL - SECOND, &mut shell, false);
    assert!(!service.has_worker());
}

#[test]
fn a_check_the_administrator_asks_for_moves_the_schedule() {
    let host = Host::running(10, &first_release());
    host.publisher.publish(2, &[&host.running]);
    let mut shell = ScriptedHost::accepting();
    let (mut service, first) = choosing(&host, AutomaticMode::Download);

    let asked = Instant::now();
    assert_eq!(
        run(&mut service, &mut shell, Action::Check)["phase"],
        "current"
    );
    let finished = Instant::now();

    // The check that was due two minutes after the start is no longer due.
    service.tick(first, &mut shell, false);
    assert!(!service.has_worker());
    service.tick(asked + CHECK_INTERVAL - SECOND, &mut shell, false);
    assert!(!service.has_worker());
    service.tick(finished + CHECK_INTERVAL, &mut shell, false);
    assert!(service.has_worker());
}

#[test]
fn download_mode_downloads_what_a_check_finds_and_stops_at_ready() {
    let (host, update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Download);

    let ready = tick_to_rest(&mut service, &mut shell, due);
    assert_eq!(ready["phase"], "ready");
    assert!(ready["error"].is_null());
    assert_eq!(ready["update"]["digest"], update.digest.as_str());
    assert_eq!(host.cache_entries(), std::slice::from_ref(&update.digest));

    // Activation stays the administrator's decision.
    nothing_starts(&mut service, &mut shell, due);
    assert!(shell.requests.is_empty());
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "ready"
    );
    assert_eq!(
        ask(&mut service, &mut shell, Action::Activate)["phase"],
        "waiting_stream"
    );
}

#[test]
fn download_mode_also_continues_a_check_the_administrator_asked_for() {
    let (host, _update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let (mut service, _due) = choosing(&host, AutomaticMode::Download);
    ask(&mut service, &mut shell, Action::Check);
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, Instant::now())["phase"],
        "ready"
    );
    assert!(shell.requests.is_empty());
}

#[test]
fn install_mode_asks_the_host_to_activate_once_the_set_is_ready() {
    let (host, update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);

    let waiting = tick_to_rest(&mut service, &mut shell, due);
    assert_eq!(waiting["phase"], "waiting_stream");
    assert!(waiting["error"].is_null());
    assert_eq!(waiting["automatic"]["mode"], "install");
    // The host receives the request a console `activate` would have sent.
    assert_eq!(
        shell.requests,
        [HostPackageActivation {
            repository_id: REPOSITORY.into(),
            catalog_digest: host
                .store()
                .catalog(REPOSITORY, &current_target(), unix_timestamp())
                .unwrap()
                .digest()
                .into(),
            manifest_digest: update.digest.clone(),
            expected_active: host.running.digest.clone(),
            expected_revision: host.selection_revision(),
        }]
    );
    assert!(service.in_progress());

    // While the host holds the request, nothing more is scheduled.
    host.connector.take_requests();
    service.tick(due + CHECK_INTERVAL * 2, &mut shell, false);
    assert!(!service.has_worker());
    assert!(host.connector.take_requests().is_empty());
    assert_eq!(shell.requests.len(), 1);
}

#[test]
fn a_newly_chosen_mode_applies_to_the_update_as_it_stands() {
    let (host, _update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let mut service = host.service();
    run(&mut service, &mut shell, Action::Check);

    service.select_automatic(AutomaticMode::Download);
    let now = Instant::now();
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, now)["phase"],
        "ready"
    );
    assert!(shell.requests.is_empty());

    service.select_automatic(AutomaticMode::Install);
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, now)["phase"],
        "waiting_stream"
    );
    assert_eq!(shell.requests.len(), 1);

    // Turning automatic updates off withdraws nothing the host already holds.
    service.select_automatic(AutomaticMode::Off);
    let waiting = tick_to_rest(&mut service, &mut shell, now);
    assert_eq!(waiting["phase"], "waiting_stream");
    assert_eq!(
        waiting["automatic"],
        json!({"mode": "off", "next_check_unix_ms": null})
    );
    assert_eq!(shell.withdrawals, 0);
}

#[test]
fn no_automatic_step_starts_beside_other_work() {
    let (host, _update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);

    // Another install owns the cache and the host's activation request.
    service.tick(due, &mut shell, true);
    assert!(!service.has_worker());
    assert!(host.connector.take_requests().is_empty());

    // A worker is running: the check the administrator started.
    host.connector.hold();
    ask(&mut service, &mut shell, Action::Check);
    service.tick(due, &mut shell, false);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "checking"
    );
    host.connector.release();

    // The update it finds waits for the other install as well.
    retire_beside_another_install(&mut service, &mut shell, due);
    for _ in 0..3 {
        service.tick(due, &mut shell, true);
        assert!(!service.has_worker());
    }
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "available"
    );
    assert!(shell.requests.is_empty());

    // With the other install finished, the flow goes on from where it was.
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, due)["phase"],
        "waiting_stream"
    );
    assert_eq!(shell.requests.len(), 1);
}

#[test]
fn nothing_starts_once_shutdown_is_announced() {
    let (host, _update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);
    service.prepare_shutdown();
    nothing_starts(&mut service, &mut shell, due);
    assert!(host.connector.take_requests().is_empty());

    // Nor does a set that was found before the announcement go any further.
    let (mut service, due) = choosing(&host, AutomaticMode::Install);
    service.tick(due, &mut shell, false);
    retire_beside_another_install(&mut service, &mut shell, due);
    service.prepare_shutdown();
    nothing_starts(&mut service, &mut shell, due);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "available"
    );
    assert!(shell.requests.is_empty());
}

#[test]
fn a_host_with_no_source_schedules_nothing() {
    let host = Host::installed(10, &first_release());
    host.publisher.publish(2, &[&host.running]);
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);
    nothing_starts(&mut service, &mut shell, due + CHECK_INTERVAL);
    let status = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(status["error"], "no_source");
    assert_eq!(
        status["automatic"],
        json!({"mode": "install", "next_check_unix_ms": null})
    );
    assert!(host.connector.take_requests().is_empty());
}

#[test]
fn a_failed_check_waits_for_the_next_scheduled_one() {
    // Nothing is served at all.
    let host = Host::running(10, &first_release());
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);

    let failed = tick_to_rest(&mut service, &mut shell, due);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "catalog_download_failed");
    assert_eq!(host.connector.take_requests(), ["catalog.json"]);

    nothing_starts(&mut service, &mut shell, due);
    assert!(host.connector.take_requests().is_empty());
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["error"],
        "catalog_download_failed"
    );

    host.publisher.publish(2, &[&host.running]);
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, due + CHECK_INTERVAL)["phase"],
        "current"
    );
}

#[test]
fn a_failed_download_is_not_tried_again_before_the_next_check() {
    let (host, update) = host_with_update();
    let served = host.publisher.set_directory(&update, IMPLEMENTATION);
    fs::remove_file(served.join("amaterasu.input.so")).unwrap();
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);

    let failed = tick_to_rest(&mut service, &mut shell, due);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "download_failed");
    assert_eq!(failed["update"]["digest"], update.digest.as_str());
    host.connector.take_requests();

    nothing_starts(&mut service, &mut shell, due);
    assert!(host.connector.take_requests().is_empty());
    assert!(host.cache_entries().is_empty());
    assert!(shell.requests.is_empty());

    // The next scheduled check is the next attempt.
    host.publisher.serve(&update, IMPLEMENTATION);
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, due + CHECK_INTERVAL)["phase"],
        "waiting_stream"
    );
    assert_eq!(shell.requests.len(), 1);
}

#[test]
fn an_activation_the_host_does_not_take_is_not_asked_again_before_the_next_check() {
    for (admission, phase, error) in [
        (Admission::InProgress, "ready", None),
        (
            Admission::Unavailable,
            "failed",
            Some("activation_unavailable"),
        ),
    ] {
        let (host, _update) = host_with_update();
        let mut shell = ScriptedHost::answering(admission);
        let (mut service, due) = choosing(&host, AutomaticMode::Install);

        let stopped = tick_to_rest(&mut service, &mut shell, due);
        assert_eq!(stopped["phase"], phase);
        assert_eq!(stopped["error"].as_str(), error);
        assert_eq!(shell.requests.len(), 1);

        nothing_starts(&mut service, &mut shell, due);
        assert_eq!(shell.requests.len(), 1, "{admission:?}");

        // One more request follows the next scheduled check, and no more.
        let stopped = tick_to_rest(&mut service, &mut shell, due + CHECK_INTERVAL);
        assert_eq!(stopped["phase"], phase);
        assert_eq!(shell.requests.len(), 2, "{admission:?}");
        nothing_starts(&mut service, &mut shell, due + CHECK_INTERVAL);
        assert_eq!(shell.requests.len(), 2, "{admission:?}");
    }
}

#[test]
fn a_download_the_administrator_cancels_stays_cancelled_until_the_next_check() {
    let (host, update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Download);

    // The scheduled check finds the update, and its download is under way.
    service.tick(due, &mut shell, false);
    retire_beside_another_install(&mut service, &mut shell, due);
    host.connector.hold();
    service.tick(due, &mut shell, false);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "downloading"
    );

    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "downloading"
    );
    let cancelled = settle(&mut service, &mut shell);
    assert_eq!(cancelled["phase"], "cancelled");
    assert_eq!(cancelled["update"]["digest"], update.digest.as_str());
    host.connector.release();
    host.connector.take_requests();

    nothing_starts(&mut service, &mut shell, due);
    assert!(host.connector.take_requests().is_empty());
    assert!(host.cache_entries().is_empty());
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "cancelled"
    );

    assert_eq!(
        tick_to_rest(&mut service, &mut shell, due + CHECK_INTERVAL)["phase"],
        "ready"
    );
}

#[test]
fn an_activation_the_administrator_withdraws_is_not_asked_again_before_the_next_check() {
    let (host, _update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, due)["phase"],
        "waiting_stream"
    );

    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "ready"
    );
    assert_eq!(shell.withdrawals, 1);
    nothing_starts(&mut service, &mut shell, due);
    assert_eq!(shell.requests.len(), 1);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "ready"
    );

    assert_eq!(
        tick_to_rest(&mut service, &mut shell, due + CHECK_INTERVAL)["phase"],
        "waiting_stream"
    );
    assert_eq!(shell.requests.len(), 2);
}

#[test]
fn work_the_administrator_starts_again_continues_on_its_own() {
    let (host, update) = host_with_update();
    let served = host.publisher.set_directory(&update, IMPLEMENTATION);
    fs::remove_file(served.join("amaterasu.input.so")).unwrap();
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, due)["error"],
        "download_failed"
    );

    // The administrator repairs the repository and asks for the download.
    host.publisher.serve(&update, IMPLEMENTATION);
    ask(&mut service, &mut shell, Action::Download);
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, due)["phase"],
        "waiting_stream"
    );
    assert_eq!(shell.requests.len(), 1);
}

#[test]
fn a_set_the_host_failed_to_activate_is_left_to_the_administrator() {
    let (host, update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    {
        let (mut service, due) = choosing(&host, AutomaticMode::Install);
        assert_eq!(
            tick_to_rest(&mut service, &mut shell, due)["phase"],
            "waiting_stream"
        );
    }

    // The host tried the set, rolled back and started the old set again.
    host.write_receipt(&update.digest, "failed");
    let mut shell = ScriptedHost::accepting();
    let (mut restarted, due) = choosing(&host, AutomaticMode::Install);
    assert_eq!(
        ask(&mut restarted, &mut shell, Action::Status)["error"],
        "activation_failed"
    );

    // The scheduled check finds the same set. It is not asked for again,
    // and the failure nobody has seen yet stays reported.
    host.connector.take_requests();
    let failed = tick_to_rest(&mut restarted, &mut shell, due);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "activation_failed");
    assert_eq!(failed["update"]["digest"], update.digest.as_str());
    assert!(failed["checked_unix_ms"].is_u64());
    nothing_starts(&mut restarted, &mut shell, due);
    assert!(shell.requests.is_empty());

    // The administrator's own check reads as it always has, and the set
    // still goes no further than they take it.
    assert_eq!(
        run(&mut restarted, &mut shell, Action::Check)["phase"],
        "available"
    );
    restarted.tick(Instant::now(), &mut shell, false);
    assert!(!restarted.has_worker());
    assert_eq!(
        run(&mut restarted, &mut shell, Action::Download)["phase"],
        "ready"
    );
    restarted.tick(Instant::now(), &mut shell, false);
    assert!(shell.requests.is_empty());

    // A newer set is a new attempt.
    let repaired = host.publisher.sign_set(12, &third_release());
    host.publisher
        .publish(3, &[&host.running, &update, &repaired]);
    let waiting = tick_to_rest(&mut restarted, &mut shell, Instant::now() + CHECK_INTERVAL);
    assert_eq!(waiting["phase"], "waiting_stream");
    assert_eq!(waiting["update"]["digest"], repaired.digest.as_str());
    assert_eq!(shell.requests.len(), 1);
    assert_eq!(shell.requests[0].manifest_digest, repaired.digest);
}

/// A set after the second release, as a repaired release would be.
fn third_release() -> Vec<Module> {
    vec![
        module(SLOT, "0.2.1", b"runtime library, third build"),
        module("amaterasu.display", "0.1.0", b"display library"),
        module("amaterasu.input", "0.2.0", b"input library, second build"),
    ]
}

#[test]
fn a_set_the_host_started_on_and_never_finished_is_not_asked_for_unattended() {
    let (host, update) = host_with_update();
    // The host began the activation and its process ended before it could
    // record how that went. The old set is running again.
    host.write_receipt(&update.digest, "activating");
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "idle"
    );

    // The set is prepared and goes no further on its own.
    let ready = tick_to_rest(&mut service, &mut shell, due);
    assert_eq!(ready["phase"], "ready");
    assert_eq!(ready["update"]["digest"], update.digest.as_str());
    nothing_starts(&mut service, &mut shell, due);
    assert!(shell.requests.is_empty());

    // The administrator may still ask for it.
    assert_eq!(
        ask(&mut service, &mut shell, Action::Activate)["phase"],
        "waiting_stream"
    );
    assert_eq!(shell.requests.len(), 1);
}

#[test]
fn nothing_is_activated_unattended_after_the_host_recovered_an_interrupted_activation() {
    let (host, update) = host_with_update();
    // A rollback at startup leaves a receipt that names the running set, so
    // the set that failed is not known.
    host.write_receipt(&host.running.digest, "failed");
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);

    let ready = tick_to_rest(&mut service, &mut shell, due);
    assert_eq!(ready["phase"], "ready");
    assert_eq!(ready["update"]["digest"], update.digest.as_str());
    nothing_starts(&mut service, &mut shell, due);
    assert!(shell.requests.is_empty());

    // Not even a newer set, which may be the one that failed.
    let newer = host.publisher.sign_set(12, &third_release());
    host.publisher.publish(3, &[&host.running, &update, &newer]);
    let ready = tick_to_rest(&mut service, &mut shell, due + CHECK_INTERVAL);
    assert_eq!(ready["phase"], "ready");
    assert_eq!(ready["update"]["digest"], newer.digest.as_str());
    assert!(shell.requests.is_empty());

    // An activation that ends well replaces the receipt, and the next
    // module instance asks unattended again.
    host.write_receipt(&host.running.digest, "committed");
    let (mut restarted, due) = choosing(&host, AutomaticMode::Install);
    assert_eq!(
        tick_to_rest(&mut restarted, &mut shell, due)["phase"],
        "waiting_stream"
    );
    assert_eq!(shell.requests.len(), 1);
    assert_eq!(shell.requests[0].manifest_digest, newer.digest);
}

#[test]
fn another_flows_failure_does_not_hold_back_a_different_set() {
    let (host, update) = host_with_update();
    // The host failed to activate a set this update does not offer.
    host.write_receipt(&"d".repeat(64), "failed");
    let mut shell = ScriptedHost::accepting();
    let (mut service, due) = choosing(&host, AutomaticMode::Install);
    assert_eq!(
        tick_to_rest(&mut service, &mut shell, due)["phase"],
        "waiting_stream"
    );
    assert_eq!(shell.requests[0].manifest_digest, update.digest);
}
