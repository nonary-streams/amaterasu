//! The retained sets a host lists, and the request to return to one.
use super::super::switching_record;
use super::super::versions::{VersionRequest, Versions};
use super::*;
use amaterasu_package::NativeReleaseFloor;

/// Release 11: the first release with a rebuilt input library.
fn interim_release() -> Vec<Module> {
    vec![
        module(SLOT, "0.1.0", b"runtime library, first build"),
        module("amaterasu.display", "0.1.0", b"display library"),
        module("amaterasu.input", "0.1.0", b"input library, rebuilt"),
    ]
}

/// Release 13, which a repository offers the host that runs release 12.
pub(super) fn next_release() -> Vec<Module> {
    vec![
        module(SLOT, "0.3.0", b"runtime library, third build"),
        module("amaterasu.display", "0.1.0", b"display library"),
        module("amaterasu.input", "0.2.0", b"input library, second build"),
        module("amaterasu.stats", "0.1.0", b"statistics library"),
    ]
}

/// A host that ran releases 10 and 11 and now runs 12.
pub(super) struct Retaining {
    pub(super) host: Host,
    pub(super) oldest: SignedSet,
    pub(super) previous: SignedSet,
}

impl Retaining {
    pub(super) fn new() -> Self {
        let host = Host::running(12, &second_release());
        let oldest = host.publisher.sign_set(10, &first_release());
        let previous = host.publisher.sign_set(11, &interim_release());
        let retaining = Self {
            host,
            oldest,
            previous,
        };
        retaining.retain(&retaining.oldest);
        retaining.retain(&retaining.previous);
        let mut selection = retaining.host.selection();
        selection.previous = Some(retaining.previous.digest.clone());
        selection
            .healthy_sets
            .insert(retaining.host.running.digest.clone());
        selection.repository_floors.insert(
            retaining.host.publisher.fingerprint(),
            NativeReleaseFloor {
                sequence: 12,
                digest: retaining.host.running.digest.clone(),
            },
        );
        retaining.host.write_selection(&selection);
        retaining
    }

    /// Records `set` as one this host ran healthy, with its files on disk.
    fn retain(&self, set: &SignedSet) {
        set.write_to(&package_directory(&self.host.root, &set.digest).unwrap());
        let mut selection = self.host.selection();
        selection.healthy_sets.insert(set.digest.clone());
        selection.repository_sets.insert(
            set.digest.clone(),
            RepositorySet {
                repository_id: REPOSITORY.into(),
                key_fingerprint: self.host.publisher.fingerprint(),
            },
        );
        self.host.write_selection(&selection);
    }
}

/// The listing once the module's first read has finished.
pub(super) fn listing(service: &mut TestService, shell: &mut ScriptedHost) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let reply = service.versions(VersionRequest::List, shell, false);
        if reply.get("loading").is_none() {
            return reply;
        }
        assert!(Instant::now() < deadline, "listing was never read: {reply}");
        thread::sleep(Duration::from_millis(1));
    }
}

fn listed<'a>(listing: &'a Value, digest: &str) -> &'a Value {
    listing["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["digest"] == digest)
        .unwrap_or_else(|| panic!("{digest} is not listed in {listing}"))
}

pub(super) fn restore(service: &mut TestService, shell: &mut ScriptedHost, digest: &str) -> Value {
    service.versions(
        VersionRequest::Restore {
            digest: digest.into(),
        },
        shell,
        false,
    )
}

/// A service whose listing has been read.
pub(super) fn listing_service(host: &Host, shell: &mut ScriptedHost) -> TestService {
    let mut service = host.service();
    listing(&mut service, shell);
    service
}

#[test]
fn the_listing_names_the_retained_sets_newest_first_with_what_differs() {
    let retaining = Retaining::new();
    let mut shell = ScriptedHost::accepting();
    let mut service = retaining.host.service();
    let reply = listing(&mut service, &mut shell);
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["supported"], true);
    assert_eq!(reply["total"], 3);
    let versions = reply["versions"].as_array().unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|entry| entry["digest"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            retaining.host.running.digest.as_str(),
            retaining.previous.digest.as_str(),
            retaining.oldest.digest.as_str(),
        ]
    );
    let now = unix_milliseconds();
    for (entry, sequence, state, modules, differs, restorable) in [
        (&versions[0], "12", "running", 4, 0, false),
        // Against the running set: runtime and input differ and statistics
        // is missing.
        (&versions[1], "11", "previous", 3, 3, true),
        (&versions[2], "10", "retained", 3, 3, true),
    ] {
        // A release sequence is text so a browser keeps all 64 bits.
        assert_eq!(entry["sequence"], sequence);
        assert_eq!(entry["state"], state);
        assert_eq!(entry["modules"], modules);
        assert_eq!(entry["differs"], differs);
        assert_eq!(entry["restorable"], restorable);
        assert!(entry["reason"].is_null(), "{entry}");
        let installed = entry["installed_unix_ms"].as_u64().unwrap();
        assert!(installed <= now && installed > now - 600_000, "{entry}");
        assert!(entry["switches_back"].is_boolean(), "{entry}");
        assert_eq!(entry.as_object().unwrap().len(), 9, "{entry}");
    }
    // Listing changes nothing and asks the host for nothing.
    assert!(shell.restore_requests.is_empty());
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "idle"
    );
}

#[test]
fn a_set_the_host_would_refuse_is_listed_with_the_reason() {
    let retaining = Retaining::new();
    let host = &retaining.host;
    let expired = host
        .publisher
        .sign_set_until(9, &first_release(), unix_timestamp() - 1);
    retaining.retain(&expired);
    // On disk and healthy, with no record of who published it.
    let unattributed = host.publisher.sign_set(8, &first_release());
    retaining.retain(&unattributed);
    // Recorded under a key other than the one the running set came from.
    let foreign = host.publisher.sign_set(7, &first_release());
    retaining.retain(&foreign);
    // Recorded as healthy, but its files are gone.
    let removed = host.publisher.sign_set(6, &first_release());
    retaining.retain(&removed);
    fs::remove_dir_all(package_directory(&host.root, &removed.digest).unwrap()).unwrap();
    // Another set claiming the accepted release's own number.
    let rival = host.publisher.sign_set(12, &interim_release());
    retaining.retain(&rival);
    let mut selection = host.selection();
    selection.repository_sets.remove(&unattributed.digest);
    selection
        .repository_sets
        .get_mut(&foreign.digest)
        .unwrap()
        .key_fingerprint = "f".repeat(64);
    host.write_selection(&selection);

    let mut shell = ScriptedHost::accepting();
    let mut service = host.service();
    let reply = listing(&mut service, &mut shell);
    // The set whose files are gone is neither listed nor counted.
    assert_eq!(reply["total"], 7);
    assert_eq!(reply["versions"].as_array().unwrap().len(), 7);
    assert!(
        reply["versions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["digest"] != removed.digest.as_str())
    );
    for (set, reason) in [
        (&expired, "expired"),
        (&unattributed, "unavailable"),
        (&foreign, "publisher_untrusted"),
        (&rival, "unavailable"),
    ] {
        let entry = listed(&reply, &set.digest);
        assert_eq!(entry["restorable"], false, "{entry}");
        assert_eq!(entry["reason"], reason, "{entry}");
        assert_eq!(
            refusal(&restore(&mut service, &mut shell, &set.digest)),
            "not_restorable"
        );
    }
    assert_eq!(listed(&reply, &retaining.oldest.digest)["restorable"], true);
    assert!(shell.restore_requests.is_empty());

    // A publisher the administrator has removed since is no longer one the
    // host would run, and the next read says so.
    host.store().revoke(REPOSITORY).unwrap();
    service.reread_versions();
    let reply = listing(&mut service, &mut shell);
    for set in [&retaining.oldest, &retaining.previous] {
        let entry = listed(&reply, &set.digest);
        assert_eq!(entry["restorable"], false);
        assert_eq!(entry["reason"], "publisher_untrusted");
    }
    let running = listed(&reply, &host.running.digest);
    assert_eq!(running["state"], "running");
    assert_eq!(running["restorable"], false);
    assert!(running["reason"].is_null());
    assert_eq!(
        refusal(&restore(&mut service, &mut shell, &retaining.oldest.digest)),
        "not_restorable"
    );
}

#[test]
fn the_listing_holds_the_newest_sets_and_always_the_running_one() {
    // The host was returned to its oldest set: every other one is newer.
    let host = Host::running(u64::MAX - 40, &first_release());
    let retaining = Retaining {
        oldest: host.publisher.sign_set(1, &first_release()),
        previous: host.publisher.sign_set(2, &first_release()),
        host,
    };
    let host = &retaining.host;
    let newer: Vec<SignedSet> = (1..=30)
        .map(|step| {
            host.publisher
                .sign_set(u64::MAX - 40 + step, &second_release())
        })
        .collect();
    for set in &newer {
        retaining.retain(set);
    }
    let mut selection = host.selection();
    selection.healthy_sets.insert(host.running.digest.clone());
    selection.highest_sequence = u64::MAX - 10;
    selection.repository_floors.insert(
        host.publisher.fingerprint(),
        NativeReleaseFloor {
            sequence: u64::MAX - 10,
            digest: newer[29].digest.clone(),
        },
    );
    host.write_selection(&selection);

    let mut shell = ScriptedHost::accepting();
    let mut service = host.service();
    let reply = listing(&mut service, &mut shell);
    assert_eq!(reply["total"], 31);
    let versions = reply["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 24);
    // The 23 newest, then the running set in last place.
    for (entry, set) in versions.iter().zip(newer.iter().rev()).take(23) {
        assert_eq!(entry["digest"], set.digest.as_str());
        assert_eq!(entry["state"], "retained");
        assert_eq!(entry["restorable"], true);
    }
    assert_eq!(versions[0]["sequence"], (u64::MAX - 10).to_string());
    assert_eq!(versions[23]["digest"], host.running.digest.as_str());
    assert_eq!(versions[23]["state"], "running");
    // A full listing of the largest numbers stays far inside one API reply.
    assert!(reply.to_string().len() < crate::http::MAX_RESPONSE / 2);
    // A set too old to be listed is not one the console can ask for.
    assert_eq!(
        refusal(&restore(&mut service, &mut shell, &newer[0].digest)),
        "not_restorable"
    );
}

#[test]
fn a_restore_is_held_by_the_host_and_can_be_withdrawn() {
    let retaining = Retaining::new();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    assert!(ask(&mut service, &mut shell, Action::Status)["restoring"].is_null());

    let waiting = restore(&mut service, &mut shell, &retaining.oldest.digest);
    assert_eq!(waiting["ok"], true, "{waiting}");
    assert_eq!(waiting["phase"], "waiting_stream");
    assert!(waiting["error"].is_null());
    assert_eq!(
        waiting["restoring"],
        serde_json::json!({"digest": retaining.oldest.digest, "sequence": "10"})
    );
    // The host is asked for exactly that set, from this selection.
    assert_eq!(
        shell.restore_requests,
        [HostPackageRestore {
            manifest_digest: retaining.oldest.digest.clone(),
            expected_active: host.running.digest.clone(),
            expected_revision: host.selection_revision(),
        }]
    );
    assert!(shell.requests.is_empty());
    assert!(service.in_progress());
    // While the host holds it, nothing else may start.
    for action in [Action::Check, Action::Download, Action::Activate] {
        assert_eq!(
            refusal(&ask(&mut service, &mut shell, action)),
            "activation_in_progress"
        );
    }
    assert_eq!(
        refusal(&restore(
            &mut service,
            &mut shell,
            &retaining.previous.digest
        )),
        "activation_in_progress"
    );

    // Withdrawn, the update stands where it stood before: not checked.
    let withdrawn = ask(&mut service, &mut shell, Action::Cancel);
    assert_eq!(withdrawn["phase"], "idle");
    assert!(withdrawn["restoring"].is_null());
    assert!(withdrawn["error"].is_null());
    assert_eq!(shell.withdrawals, 1);
    assert!(!service.in_progress());

    // Asked again, the host takes it once streams have ended.
    let waiting = restore(&mut service, &mut shell, &retaining.previous.digest);
    assert_eq!(waiting["phase"], "waiting_stream");
    assert_eq!(waiting["restoring"]["sequence"], "11");
    shell.report(false, true);
    let activating = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(activating["phase"], "activating");
    assert_eq!(
        activating["restoring"]["digest"],
        retaining.previous.digest.as_str()
    );
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Cancel)),
        "activation_in_progress"
    );
    assert_eq!(shell.restore_requests.len(), 2);
}

#[test]
fn a_withdrawn_restore_leaves_a_downloaded_update_ready() {
    let retaining = Retaining::new();
    let host = &retaining.host;
    let update = host.publisher.sign_set(13, &next_release());
    host.publisher.publish(2, &[&host.running, &update]);
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    let checked = run(&mut service, &mut shell, Action::Check);
    assert_eq!(checked["phase"], "available", "{checked}");
    assert_eq!(
        run(&mut service, &mut shell, Action::Download)["phase"],
        "ready"
    );

    let waiting = restore(&mut service, &mut shell, &retaining.oldest.digest);
    assert_eq!(waiting["phase"], "waiting_stream");
    assert_eq!(
        waiting["restoring"]["digest"],
        retaining.oldest.digest.as_str()
    );
    let withdrawn = ask(&mut service, &mut shell, Action::Cancel);
    assert_eq!(withdrawn["phase"], "ready");
    assert!(withdrawn["restoring"].is_null());
    assert_eq!(withdrawn["update"]["digest"], update.digest.as_str());
    // The update the administrator downloaded can still be installed.
    let installing = ask(&mut service, &mut shell, Action::Activate);
    assert_eq!(installing["phase"], "waiting_stream");
    assert!(installing["restoring"].is_null());
    assert_eq!(shell.requests.len(), 1);
    // And withdrawing that one returns to ready, as it always did.
    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "ready"
    );
}

#[test]
fn a_restore_is_refused_when_it_cannot_be_asked_for() {
    let retaining = Retaining::new();
    let host = &retaining.host;
    let target = retaining.oldest.digest.clone();

    // A digest the host does not list, the running set, and a malformed one.
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    for digest in [
        "e".repeat(64),
        host.running.digest.clone(),
        "previous".into(),
    ] {
        assert_eq!(
            refusal(&restore(&mut service, &mut shell, &digest)),
            "not_restorable"
        );
    }

    // Another install owns the cache and the host's one request.
    assert_eq!(
        refusal(&service.versions(
            VersionRequest::Restore {
                digest: target.clone()
            },
            &mut shell,
            true,
        )),
        "busy"
    );
    // A check is running.
    let update = host.publisher.sign_set(13, &next_release());
    host.publisher.publish(2, &[&host.running, &update]);
    host.connector.hold();
    assert_eq!(
        ask(&mut service, &mut shell, Action::Check)["phase"],
        "checking"
    );
    assert_eq!(refusal(&restore(&mut service, &mut shell, &target)), "busy");
    host.connector.release();
    assert_eq!(settle(&mut service, &mut shell)["phase"], "available");

    // An update the host already holds.
    assert_eq!(
        run(&mut service, &mut shell, Action::Download)["phase"],
        "ready"
    );
    assert_eq!(
        ask(&mut service, &mut shell, Action::Activate)["phase"],
        "waiting_stream"
    );
    assert_eq!(
        refusal(&restore(&mut service, &mut shell, &target)),
        "activation_in_progress"
    );
    assert_eq!(
        ask(&mut service, &mut shell, Action::Cancel)["phase"],
        "ready"
    );
    assert!(shell.restore_requests.is_empty());

    // The host holds a request this module did not make.
    let mut held = ScriptedHost::answering(Admission::InProgress);
    assert_eq!(
        refusal(&restore(&mut service, &mut held, &target)),
        "activation_in_progress"
    );
    assert_eq!(
        ask(&mut service, &mut held, Action::Status)["phase"],
        "ready"
    );

    // The module is being unloaded.
    service.prepare_shutdown();
    assert_eq!(
        refusal(&restore(&mut service, &mut shell, &target)),
        "shutting_down"
    );
}

#[test]
fn a_host_that_takes_no_restores_offers_none() {
    let retaining = Retaining::new();
    let mut shell = ScriptedHost::accepting();
    shell.restores = false;
    let mut service = retaining.host.service();
    assert_eq!(
        service.versions(VersionRequest::List, &mut shell, false),
        serde_json::json!({"ok": true, "supported": false, "total": 0, "versions": []})
    );
    assert_eq!(
        refusal(&restore(&mut service, &mut shell, &retaining.oldest.digest)),
        "unsupported_platform"
    );
    assert!(shell.restore_requests.is_empty());
    // A host root that cannot be read lists nothing either.
    let mut blind = Service::new(
        retaining.host.connector.clone(),
        Environment::load(None, None, None),
    );
    let mut shell = ScriptedHost::accepting();
    assert_eq!(listing(&mut blind, &mut shell)["supported"], false);
    assert_eq!(
        refusal(&restore(&mut blind, &mut shell, &retaining.oldest.digest)),
        "not_restorable"
    );
}

#[test]
fn a_host_that_does_not_take_the_request_fails_the_restore() {
    let retaining = Retaining::new();
    let host = &retaining.host;
    let target = &retaining.oldest.digest;

    let mut unavailable = ScriptedHost::answering(Admission::Unavailable);
    let mut service = listing_service(host, &mut unavailable);
    let failed = restore(&mut service, &mut unavailable, target);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "activation_unavailable");
    assert!(failed["restoring"].is_null());
    assert!(!service.in_progress());

    // The request the host held is gone and was never started.
    let mut shell = ScriptedHost::accepting();
    let mut service = listing_service(host, &mut shell);
    assert_eq!(
        restore(&mut service, &mut shell, target)["phase"],
        "waiting_stream"
    );
    shell.report(false, false);
    let interrupted = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(interrupted["phase"], "failed");
    assert_eq!(interrupted["error"], "activation_interrupted");
    assert!(interrupted["restoring"].is_null());

    // The selection moved on under a module that is still loaded.
    let mut service = listing_service(host, &mut shell);
    let mut selection = host.selection();
    selection.pending = Some(retaining.previous.digest.clone());
    host.write_selection(&selection);
    assert_eq!(
        refusal(&restore(&mut service, &mut shell, target)),
        "activation_in_progress"
    );
    selection.pending = None;
    selection.previous = selection.active.replace(retaining.previous.digest.clone());
    host.write_selection(&selection);
    let stale = restore(&mut service, &mut shell, target);
    assert_eq!(stale["phase"], "failed");
    assert_eq!(stale["error"], "activation_unavailable");
    assert_eq!(shell.restore_requests.len(), 1);
}

#[test]
fn the_next_start_reports_how_the_restore_ended() {
    let retaining = Retaining::new();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();

    // The host could not return to the set and runs the one it ran before.
    host.write_receipt(&retaining.oldest.digest, "failed");
    let mut service = host.service();
    let failed = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "activation_failed");
    assert_eq!(failed["running"]["digest"], host.running.digest.as_str());
    assert!(failed["restoring"].is_null());

    // A receipt for the running set is a restore that completed.
    host.write_receipt(&host.running.digest, "committed");
    let mut service = host.service();
    let idle = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(idle["phase"], "idle");
    assert!(idle["error"].is_null());

    // A failed receipt for a set this host never ran is not this flow's.
    host.write_receipt(&"e".repeat(64), "failed");
    let mut service = host.service();
    assert_eq!(
        ask(&mut service, &mut shell, Action::Status)["phase"],
        "idle"
    );
}

/// The service of a set as it runs on a host that takes restores: started,
/// its retained sets read, and the set recorded as one that switches sets.
fn switching_service(host: &Host, running: &str, shell: &mut ScriptedHost) -> TestService {
    let mut service = Service::new(
        host.connector.clone(),
        Environment::load(
            Some(host.repository_directory()),
            Some(host.root.clone()),
            Some(running.to_owned()),
        ),
    );
    service.observe_host(shell);
    service.settle_versions();
    service
}

fn record_file(host: &Host) -> PathBuf {
    host.repository_directory().join(switching_record::FILE)
}

fn recorded_sets(host: &Host) -> Vec<String> {
    let record: Value = serde_json::from_slice(&fs::read(record_file(host)).unwrap()).unwrap();
    assert_eq!(record["schema"], 1);
    record["sets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|digest| digest.as_str().unwrap().to_owned())
        .collect()
}

/// Which listed sets say their own console can switch sets again.
fn switching_back(listing: &Value) -> Vec<(String, bool)> {
    listing["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["digest"].as_str().unwrap().to_owned(),
                entry["switches_back"].as_bool().unwrap(),
            )
        })
        .collect()
}

/// What a module lists before any host has told it that it takes restores.
fn listed_without_recording(host: &Host) -> Value {
    let mut versions = Versions::start(&host.environment());
    versions.settle();
    versions.reply(unix_timestamp())
}

#[test]
fn a_set_that_ran_with_switching_says_its_console_can_switch_back() {
    let retaining = Retaining::new();
    let host = &retaining.host;
    let [running, previous, oldest] = [
        host.running.digest.clone(),
        retaining.previous.digest.clone(),
        retaining.oldest.digest.clone(),
    ];
    // Nothing has recorded itself: no set is known to switch back.
    assert!(!record_file(host).exists());
    assert_eq!(
        switching_back(&listed_without_recording(host)),
        [
            (running.clone(), false),
            (previous.clone(), false),
            (oldest.clone(), false)
        ]
    );
    assert!(!record_file(host).exists());

    // The previous set ran here with these routes; the oldest never did.
    let mut shell = ScriptedHost::accepting();
    drop(switching_service(host, &previous, &mut shell));
    assert_eq!(recorded_sets(host), [previous.as_str()]);

    // The running set starts. Until the host has said that it takes
    // restores, it has not recorded itself.
    let mut service = host.service();
    service.settle_versions();
    assert!(service.probing_host());
    assert_eq!(recorded_sets(host), [previous.as_str()]);
    let mut unready = ScriptedHost::accepting();
    unready.restores = false;
    service.observe_host(&mut unready);
    service.settle_versions();
    assert_eq!(recorded_sets(host), [previous.as_str()]);
    assert!(service.probing_host());
    // It asks again later, not on every tick.
    for _ in 0..100 {
        service.observe_host(&mut unready);
    }
    assert_eq!(unready.support_queries, 1);

    // The console asking is another moment to find out.
    let reply = listing(&mut service, &mut shell);
    assert!(!service.probing_host());
    service.settle_versions();
    assert_eq!(recorded_sets(host), [previous.clone(), running.clone()]);
    // That reply was read before the running set recorded itself.
    assert_eq!(listed(&reply, &previous)["switches_back"], true);
    assert_eq!(listed(&reply, &oldest)["switches_back"], false);
    let reply = listing(&mut service, &mut shell);
    assert_eq!(
        switching_back(&reply),
        [
            (running.clone(), true),
            (previous.clone(), true),
            (oldest.clone(), false)
        ]
    );
    // It is a fact about what ran, and changes nothing the host accepts: the
    // set that cannot switch back is as restorable as the one that can.
    for digest in [&previous, &oldest] {
        assert_eq!(listed(&reply, digest)["restorable"], true);
        assert!(listed(&reply, digest)["reason"].is_null());
    }
    let waiting = restore(&mut service, &mut shell, &oldest);
    assert_eq!(waiting["phase"], "waiting_stream", "{waiting}");
    // Once recorded, the service asks the host nothing more on its ticks.
    let asked = shell.support_queries;
    for _ in 0..100 {
        service.observe_host(&mut shell);
    }
    assert_eq!(shell.support_queries, asked);
}

#[test]
fn the_record_outlives_the_service_that_wrote_it() {
    let retaining = Retaining::new();
    let host = &retaining.host;
    let mut shell = ScriptedHost::accepting();
    drop(switching_service(host, &host.running.digest, &mut shell));
    assert_eq!(recorded_sets(host), [host.running.digest.as_str()]);
    let written = fs::metadata(record_file(host)).unwrap().modified().unwrap();

    // The module starts again on the same set: it reads what it wrote, on a
    // start that has not yet heard from the host.
    let reply = listed_without_recording(host);
    assert_eq!(listed(&reply, &host.running.digest)["switches_back"], true);
    assert_eq!(
        listed(&reply, &retaining.previous.digest)["switches_back"],
        false
    );
    // Recording itself again changes nothing on disk.
    drop(switching_service(host, &host.running.digest, &mut shell));
    assert_eq!(recorded_sets(host), [host.running.digest.as_str()]);
    assert_eq!(
        fs::metadata(record_file(host)).unwrap().modified().unwrap(),
        written
    );

    // The host moves to another set. The one that recorded itself is still
    // known to switch back, though its module is no longer loaded.
    let mut service = switching_service(host, &retaining.previous.digest, &mut shell);
    assert_eq!(
        recorded_sets(host),
        [
            host.running.digest.clone(),
            retaining.previous.digest.clone()
        ]
    );
    let reply = listing(&mut service, &mut shell);
    assert_eq!(listed(&reply, &host.running.digest)["switches_back"], true);
    assert_eq!(
        listed(&reply, &retaining.previous.digest)["switches_back"],
        true
    );
    assert_eq!(
        listed(&reply, &retaining.oldest.digest)["switches_back"],
        false
    );
}

#[test]
fn the_record_keeps_only_the_sets_that_ran_most_recently() {
    let retaining = Retaining::new();
    let host = &retaining.host;
    let earlier: Vec<String> = (0..switching_record::CAPACITY)
        .map(|number| format!("{number:064x}"))
        .collect();
    fs::create_dir_all(host.repository_directory()).unwrap();
    write_json_atomically(
        &record_file(host),
        &serde_json::json!({"schema": 1, "sets": earlier}),
    )
    .unwrap();

    let mut shell = ScriptedHost::accepting();
    let mut service = switching_service(host, &host.running.digest, &mut shell);
    let sets = recorded_sets(host);
    assert_eq!(sets.len(), switching_record::CAPACITY);
    // The set that ran longest ago made room for the one running now.
    assert_eq!(sets[..switching_record::CAPACITY - 1], earlier[1..]);
    assert_eq!(sets.last(), Some(&host.running.digest));
    assert!(fs::metadata(record_file(host)).unwrap().len() < 32 * 1024);
    let reply = listing(&mut service, &mut shell);
    assert_eq!(listed(&reply, &host.running.digest)["switches_back"], true);
}

#[test]
fn a_record_that_cannot_be_used_lists_no_set_as_switching_back() {
    let none_switch = |reply: &Value| {
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["supported"], true, "{reply}");
        assert_eq!(reply["total"], 3, "{reply}");
        let sets = switching_back(reply);
        assert_eq!(sets.len(), 3);
        assert!(sets.iter().all(|(_, switches)| !switches), "{reply}");
    };

    // A file that is not a record. The listing is whole, and every set says
    // false. The next set to record itself replaces the file.
    for contents in [
        b"{ not a record".as_slice(),
        br#"{"schema":9,"sets":[]}"#,
        br#"{"schema":1,"sets":["previous"]}"#,
        b"",
    ] {
        let retaining = Retaining::new();
        let host = &retaining.host;
        fs::create_dir_all(host.repository_directory()).unwrap();
        fs::write(record_file(host), contents).unwrap();
        none_switch(&listed_without_recording(host));
        assert_eq!(fs::read(record_file(host)).unwrap(), contents);

        let mut shell = ScriptedHost::accepting();
        let mut service = switching_service(host, &host.running.digest, &mut shell);
        assert_eq!(recorded_sets(host), [host.running.digest.as_str()]);
        let reply = listing(&mut service, &mut shell);
        assert_eq!(
            switching_back(&reply),
            [
                (host.running.digest.clone(), true),
                (retaining.previous.digest.clone(), false),
                (retaining.oldest.digest.clone(), false),
            ]
        );
    }

    // Something that cannot be read as a file. Every set says false, the
    // running one included, nothing is written over it, and the listing and
    // a restore work as they would without a record.
    let retaining = Retaining::new();
    let host = &retaining.host;
    fs::create_dir_all(record_file(host)).unwrap();
    none_switch(&listed_without_recording(host));
    let mut shell = ScriptedHost::accepting();
    let mut service = switching_service(host, &host.running.digest, &mut shell);
    let reply = listing(&mut service, &mut shell);
    none_switch(&reply);
    assert!(record_file(host).is_dir());
    assert_eq!(listed(&reply, &retaining.oldest.digest)["restorable"], true);
    assert_eq!(
        restore(&mut service, &mut shell, &retaining.oldest.digest)["phase"],
        "waiting_stream"
    );

    // A host with no repository directory keeps no record.
    let mut versions = Versions::start(&Environment::load(
        None,
        Some(host.root.clone()),
        Some(host.running.digest.clone()),
    ));
    versions.host_switches();
    versions.settle();
    none_switch(&versions.reply(unix_timestamp()));
}
