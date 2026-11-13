//! A return to a retained set and automatic updates on one host: neither
//! starts beside the other, and withdrawing one leaves the other as it was.
use super::super::switching_record;
use super::versions::{
    AUTOMATIC_OFF, AUTOMATIC_ON, Retaining, SETTINGS_UNREAD, listing, listing_service,
    next_release, restore, restore_with_settings,
};
use super::*;
use crate::package_update::automatic::{AutomaticMode, CHECK_INTERVAL, FIRST_CHECK_DELAY};

/// A host that retains two earlier sets and whose repository offers a newer
/// one than it runs.
fn retaining_host_with_update() -> (Retaining, SignedSet) {
    let retaining = Retaining::new();
    let update = retaining.host.publisher.sign_set(13, &next_release());
    retaining
        .host
        .publisher
        .publish(2, &[&retaining.host.running, &update]);
    (retaining, update)
}

/// Ticks through the hours after `since`, long past every scheduled check,
/// and expects no worker and no request to the host.
fn nothing_automatic_starts(service: &mut TestService, shell: &mut ScriptedHost, since: Instant) {
    let asked = shell.requests.len();
    for minutes in [0, 1, 2, 30, 360, 720] {
        service.tick(since + Duration::from_secs(minutes * 60), shell, false);
        assert!(!service.has_worker(), "after {minutes} minutes");
    }
    assert_eq!(shell.requests.len(), asked);
}

#[test]
fn no_automatic_check_starts_while_a_restore_waits_or_runs() {
    let (retaining, _update) = retaining_host_with_update();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    service.select_automatic(AutomaticMode::Install);
    let first_check = Instant::now() + FIRST_CHECK_DELAY;

    assert_eq!(
        restore(&mut service, &mut shell, &retaining.oldest.digest)["phase"],
        "waiting_stream"
    );
    host.connector.take_requests();
    // The scheduled check comes due while the host waits for streams to end.
    nothing_automatic_starts(&mut service, &mut shell, first_check);
    let waiting = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(waiting["phase"], "waiting_stream");
    assert_eq!(
        waiting["restoring"]["digest"],
        retaining.oldest.digest.as_str()
    );
    assert_eq!(waiting["automatic"]["mode"], "install");

    // And while the host replaces the set.
    shell.report(false, true);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "activating"
    );
    nothing_automatic_starts(&mut service, &mut shell, first_check + CHECK_INTERVAL);
    assert!(host.connector.take_requests().is_empty());
    assert!(shell.requests.is_empty());
    assert_eq!(shell.restore_requests.len(), 1);
}

#[test]
fn no_automatic_download_or_activation_starts_beside_a_restore() {
    let (retaining, update) = retaining_host_with_update();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    let now = Instant::now();

    // A check the administrator asked for found an update; nothing is
    // automatic yet, so it stays where it is.
    assert_eq!(
        run(&mut service, &mut shell, Action::Check)["phase"],
        "available"
    );
    assert_eq!(
        restore(&mut service, &mut shell, &retaining.oldest.digest)["phase"],
        "waiting_stream"
    );
    // Choosing a mode now would download that update at once, were the host
    // not holding the restore.
    service.select_automatic(AutomaticMode::Download);
    host.connector.take_requests();
    nothing_automatic_starts(&mut service, &mut shell, now);
    assert!(host.connector.take_requests().is_empty());
    assert!(host.cache_entries().is_empty());

    // Withdrawn, the restore leaves the update where it stood, and the
    // automatic flow goes on from there: nothing of the update's was stopped.
    let withdrawn = ask(&mut service, &mut shell, Action::Cancel);
    assert_eq!(withdrawn["phase"], "available");
    assert!(withdrawn["restoring"].is_null());
    service.tick(now, &mut shell, false);
    assert!(service.has_worker(), "the download did not continue");
    let ready = tick_to_rest(&mut service, &mut shell, now);
    assert_eq!(ready["phase"], "ready");
    assert_eq!(ready["update"]["digest"], update.digest.as_str());

    // The same with a downloaded set and `install`: while a restore is held
    // the host is not also asked to activate the update.
    assert_eq!(
        restore(&mut service, &mut shell, &retaining.oldest.digest)["phase"],
        "waiting_stream"
    );
    service.select_automatic(AutomaticMode::Install);
    nothing_automatic_starts(&mut service, &mut shell, now);
    assert!(shell.requests.is_empty());
    let withdrawn = ask(&mut service, &mut shell, Action::Cancel);
    assert_eq!(withdrawn["phase"], "ready");
    // Free again, the update is installed as the mode says.
    service.tick(now, &mut shell, false);
    assert_eq!(shell.requests.len(), 1);
    assert_eq!(shell.requests[0].manifest_digest, update.digest);
    let installing = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(installing["phase"], "waiting_stream");
    assert!(installing["restoring"].is_null());
    assert_eq!(shell.restore_requests.len(), 2);
}

#[test]
fn a_restore_is_refused_while_an_automatic_step_holds_the_host() {
    let (retaining, update) = retaining_host_with_update();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    service.select_automatic(AutomaticMode::Install);
    let first_check = Instant::now() + FIRST_CHECK_DELAY;
    let target = &retaining.oldest.digest;

    // The scheduled check.
    host.connector.hold();
    service.tick(first_check, &mut shell, false);
    assert!(service.has_worker());
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "checking"
    );
    assert_eq!(refusal(&restore(&mut service, &mut shell, target)), "busy");

    // The download that follows it. A held connector lets one tick start it
    // and keeps it running.
    host.connector.release();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        host.connector.hold();
        service.tick(first_check, &mut shell, false);
        let phase = ask(&mut service, &mut shell, Action::Status)["phase"].clone();
        if phase == "downloading" {
            break;
        }
        host.connector.release();
        assert!(Instant::now() < deadline, "no download started: {phase}");
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(refusal(&restore(&mut service, &mut shell, target)), "busy");

    // The activation the mode then asks the host for.
    host.connector.release();
    let waiting = tick_to_rest(&mut service, &mut shell, first_check);
    assert_eq!(waiting["phase"], "waiting_stream");
    assert!(waiting["restoring"].is_null());
    assert_eq!(shell.requests.len(), 1);
    assert_eq!(shell.requests[0].manifest_digest, update.digest);
    assert_eq!(
        refusal(&restore(&mut service, &mut shell, target)),
        "activation_in_progress"
    );
    shell.report(false, true);
    assert_eq!(
        refusal(&restore(&mut service, &mut shell, target)),
        "activation_in_progress"
    );
    // The host was never asked to restore anything.
    assert!(shell.restore_requests.is_empty());
    // The listing stays readable throughout.
    assert_eq!(listing(&mut service, &mut shell)["total"], 3);
}

#[test]
fn withdrawing_an_automatic_activation_and_withdrawing_a_restore_stay_apart() {
    let (retaining, update) = retaining_host_with_update();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    service.select_automatic(AutomaticMode::Install);
    let first_check = Instant::now() + FIRST_CHECK_DELAY;

    // The schedule checks, downloads and asks the host; the administrator
    // withdraws that. It is the update that was stopped: no restore appears.
    let waiting = tick_to_rest(&mut service, &mut shell, first_check);
    assert_eq!(waiting["phase"], "waiting_stream");
    let withdrawn = ask(&mut service, &mut shell, Action::Cancel);
    assert_eq!(withdrawn["phase"], "ready");
    assert!(withdrawn["restoring"].is_null());
    assert_eq!(shell.withdrawals, 1);
    service.tick(first_check, &mut shell, false);
    assert_eq!(
        shell.requests.len(),
        1,
        "a withdrawn activation was asked for again"
    );

    // A restore can now be asked for, and withdrawing it puts the update
    // back at ready without making the withdrawn activation due again.
    assert_eq!(
        restore(&mut service, &mut shell, &retaining.previous.digest)["phase"],
        "waiting_stream"
    );
    let withdrawn = ask(&mut service, &mut shell, Action::Cancel);
    assert_eq!(withdrawn["phase"], "ready");
    assert!(withdrawn["restoring"].is_null());
    assert_eq!(withdrawn["update"]["digest"], update.digest.as_str());
    assert_eq!(shell.withdrawals, 2);
    service.tick(first_check, &mut shell, false);
    service.tick(first_check + Duration::from_secs(3600), &mut shell, false);
    assert_eq!(
        shell.requests.len(),
        1,
        "withdrawing a restore restarted the update"
    );
    assert!(!service.has_worker());
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "ready"
    );

    // The next scheduled check is what moves the update on again, as it
    // would have without the restore.
    let next = tick_to_rest(&mut service, &mut shell, first_check + CHECK_INTERVAL * 2);
    assert_eq!(next["phase"], "waiting_stream");
    assert!(next["restoring"].is_null());
    assert_eq!(shell.requests.len(), 2);
    assert_eq!(shell.restore_requests.len(), 1);
}

/// A host whose previous set ran here with these routes and with automatic
/// updates, and whose oldest set never did.
fn host_with_one_set_from_before() -> Retaining {
    let retaining = Retaining::new();
    switching_record::record(
        &retaining.host.repository_directory(),
        &retaining.previous.digest,
    );
    retaining
}

#[test]
fn a_restore_to_a_set_from_before_automatic_updates_needs_them_off() {
    let retaining = host_with_one_set_from_before();
    let host = &retaining.host;
    let [before, since] = [&retaining.oldest.digest, &retaining.previous.digest];
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    service.settle_versions();
    let reply = listing(&mut service, &mut shell);
    // Both are sets the host would return to; the record tells them apart.
    for (digest, switches_back) in [(before, false), (since, true)] {
        let entry = reply["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["digest"] == digest.as_str())
            .unwrap();
        assert_eq!(entry["restorable"], true);
        assert_eq!(entry["switches_back"], switches_back);
    }

    // The settings carry `download` or `install`. The earlier set's module
    // could not read them, so the host is not even asked.
    assert_eq!(
        refusal(&restore_with_settings(
            &mut service,
            &mut shell,
            before,
            AUTOMATIC_ON
        )),
        "automatic_updates_on"
    );
    assert!(shell.restore_requests.is_empty());
    let unchanged = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(unchanged["phase"], "idle");
    assert!(unchanged["restoring"].is_null());
    assert!(!service.in_progress());
    assert!(!service.restoring_set_from_before_automatic_updates());

    // A set that ran here with them is returned to whatever the mode.
    let waiting = restore_with_settings(&mut service, &mut shell, since, AUTOMATIC_ON);
    assert_eq!(waiting["phase"], "waiting_stream", "{waiting}");
    assert_eq!(waiting["restoring"]["digest"], since.as_str());
    assert_eq!(shell.restore_requests.len(), 1);
    assert!(!service.restoring_set_from_before_automatic_updates());
    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "idle"
    );

    // With automatic updates off and saved, the earlier set is too. While
    // the host holds that restore, the module knows not to let them on.
    let waiting = restore_with_settings(&mut service, &mut shell, before, AUTOMATIC_OFF);
    assert_eq!(waiting["phase"], "waiting_stream", "{waiting}");
    assert_eq!(shell.restore_requests.len(), 2);
    assert_eq!(shell.restore_requests[1].manifest_digest, *before);
    assert!(service.restoring_set_from_before_automatic_updates());
    shell.report(false, true);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "activating"
    );
    assert!(service.restoring_set_from_before_automatic_updates());
}

#[test]
fn a_withdrawn_or_lost_restore_no_longer_keeps_automatic_updates_off() {
    let retaining = host_with_one_set_from_before();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);

    restore(&mut service, &mut shell, &retaining.oldest.digest);
    assert!(service.restoring_set_from_before_automatic_updates());
    ask(&mut service, &mut shell, Action::Cancel);
    assert!(!service.restoring_set_from_before_automatic_updates());

    restore(&mut service, &mut shell, &retaining.oldest.digest);
    assert!(service.restoring_set_from_before_automatic_updates());
    // The request the host held is gone and was never started.
    shell.report(false, false);
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["error"],
        "activation_interrupted"
    );
    assert!(!service.restoring_set_from_before_automatic_updates());
}

#[test]
fn settings_that_have_not_been_read_hold_back_only_a_set_from_before() {
    let retaining = host_with_one_set_from_before();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);

    // Nothing is known about what an earlier module would find.
    assert_eq!(
        refusal(&restore_with_settings(
            &mut service,
            &mut shell,
            &retaining.oldest.digest,
            SETTINGS_UNREAD
        )),
        "settings_unavailable"
    );
    assert!(shell.restore_requests.is_empty());
    let waiting = restore_with_settings(
        &mut service,
        &mut shell,
        &retaining.previous.digest,
        SETTINGS_UNREAD,
    );
    assert_eq!(waiting["phase"], "waiting_stream", "{waiting}");
}

#[test]
fn the_automatic_rule_comes_after_every_other_reason_to_refuse_a_restore() {
    let (retaining, _update) = retaining_host_with_update();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    let before = &retaining.oldest.digest;
    let asked = |service: &mut TestService, shell: &mut ScriptedHost, digest: &str| -> String {
        refusal(&restore_with_settings(service, shell, digest, AUTOMATIC_ON)).to_owned()
    };

    // A set the host would not return to at all is said to be that.
    assert_eq!(
        asked(&mut service, &mut shell, &"e".repeat(64)),
        "not_restorable"
    );
    assert_eq!(
        asked(&mut service, &mut shell, &host.running.digest),
        "not_restorable"
    );
    // Work in progress is reported as that work.
    host.connector.hold();
    ask(&mut service, &mut shell, Action::Check);
    assert_eq!(asked(&mut service, &mut shell, before), "busy");
    host.connector.release();
    settle(&mut service, &mut shell);
    run(&mut service, &mut shell, Action::Download);
    ask(&mut service, &mut shell, Action::Activate);
    assert_eq!(
        asked(&mut service, &mut shell, before),
        "activation_in_progress"
    );
    ask(&mut service, &mut shell, Action::Cancel);
    // A host that takes no restores says so first.
    let mut old_shell = ScriptedHost::accepting();
    old_shell.restores = false;
    assert_eq!(
        asked(&mut service, &mut old_shell, before),
        "unsupported_platform"
    );
    assert_eq!(
        asked(&mut service, &mut shell, before),
        "automatic_updates_on"
    );
    assert!(shell.restore_requests.is_empty());
}
