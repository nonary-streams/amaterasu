//! A signed repository checked and downloaded through the service into a
//! temporary host root, and the ways a repository's bytes can be wrong.
use super::*;
use amaterasu_package::repository::{verify_candidate, with_verified_production_candidate};

/// A host on the first release whose repository also offers the second.
fn host_with_update() -> (Host, SignedSet) {
    let host = Host::running(10, &first_release());
    let update = host.publisher.sign_set(11, &second_release());
    host.publisher.publish(2, &[&host.running, &update]);
    (host, update)
}

fn artifact_requests(connector: &DirectoryConnector) -> Vec<String> {
    connector
        .take_requests()
        .into_iter()
        .filter_map(|path| path.rsplit('/').next().map(str::to_owned))
        .collect()
}

#[test]
fn a_newer_signed_set_is_checked_downloaded_and_left_verified_in_the_cache() {
    let (host, update) = host_with_update();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();

    let idle = ask(&mut service, &mut shell, Action::Status);
    assert_eq!(idle["ok"], true);
    assert_eq!(idle["supported"], true);
    assert_eq!(idle["phase"], "idle");
    assert_eq!(
        idle["source"],
        serde_json::json!({
            "repository": REPOSITORY,
            "fingerprint": host.publisher.fingerprint(),
        })
    );
    assert_eq!(idle["running"]["digest"], host.running.digest.as_str());
    assert_eq!(idle["running"]["sequence"], "10");
    for absent in ["checked_unix_ms", "update", "error"] {
        assert!(idle[absent].is_null(), "{absent}");
    }
    assert_eq!(idle["transferred_bytes"], 0);

    let before = unix_milliseconds();
    let available = run(&mut service, &mut shell, Action::Check);
    assert_eq!(available["phase"], "available");
    assert!(available["error"].is_null());
    assert!(available["checked_unix_ms"].as_u64().unwrap() >= before);
    // A check reads signed metadata only.
    assert_eq!(
        artifact_requests(&host.connector),
        [
            "catalog.json",
            "catalog.sig",
            "manifest.json",
            "manifest.sig"
        ]
    );
    assert!(host.cache_entries().is_empty());

    let offered = &available["update"];
    assert_eq!(offered["repository"], REPOSITORY);
    assert_eq!(offered["slot"], SLOT);
    assert_eq!(offered["implementation"], IMPLEMENTATION);
    assert_eq!(offered["version"], "0.2.0");
    assert_eq!(offered["digest"], update.digest.as_str());
    assert_eq!(offered["sequence"], "11");
    let changed = |id: &str, from: Option<&str>, to: Option<&str>, change: &str, size: usize| serde_json::json!({"id": id, "from": from, "to": to, "change": change, "size": size});
    assert_eq!(
        offered["modules"],
        serde_json::json!([
            changed(
                "amaterasu.display",
                Some("0.1.0"),
                Some("0.1.0"),
                "unchanged",
                15
            ),
            changed(
                "amaterasu.input",
                Some("0.1.0"),
                Some("0.2.0"),
                "updated",
                27
            ),
            changed(SLOT, Some("0.1.0"), Some("0.2.0"), "updated", 29),
            changed("amaterasu.stats", None, Some("0.1.0"), "added", 18),
        ])
    );
    assert_eq!(offered["total_bytes"], 15 + 27 + 29 + 18);
    assert_eq!(offered["download_bytes"], 27 + 29 + 18);

    let downloading = ask(&mut service, &mut shell, Action::Download);
    assert_eq!(downloading["phase"], "downloading");
    let ready = settle(&mut service, &mut shell);
    assert_eq!(ready["phase"], "ready");
    assert!(ready["error"].is_null());
    assert_eq!(ready["update"]["digest"], update.digest.as_str());
    assert_eq!(ready["transferred_bytes"], 27 + 29 + 18);
    // The library the running set already holds was not fetched.
    let mut fetched = artifact_requests(&host.connector);
    fetched.sort();
    assert_eq!(
        fetched,
        [
            "amaterasu.input.so",
            "amaterasu.runtime.so",
            "amaterasu.stats.so",
            "manifest.json",
            "manifest.sig",
        ]
    );

    // The cache holds exactly the candidate, and the package verifier accepts it.
    assert_eq!(host.cache_entries(), std::slice::from_ref(&update.digest));
    let cached = host.cached(&update);
    for (name, bytes) in &update.files {
        assert_eq!(&fs::read(cached.join(name)).unwrap(), bytes, "{name}");
    }
    let store = host.store();
    let approved = store.approved(REPOSITORY).unwrap();
    let catalog = store
        .catalog(REPOSITORY, &current_target(), unix_timestamp())
        .unwrap();
    verify_candidate(
        &approved,
        &catalog,
        &update.listed(),
        &cached,
        &host.bootstrap(),
        unix_timestamp(),
    )
    .unwrap();

    // The request the host receives is exactly what its own production
    // verification accepts for this selection.
    let waiting = ask(&mut service, &mut shell, Action::Activate);
    assert_eq!(waiting["phase"], "waiting_stream");
    let request = shell.requests.pop().unwrap();
    assert_eq!(
        request,
        HostPackageActivation {
            repository_id: REPOSITORY.into(),
            catalog_digest: catalog.digest().into(),
            manifest_digest: update.digest.clone(),
            expected_active: host.running.digest.clone(),
            expected_revision: host.selection_revision(),
        }
    );
    let sequence = with_verified_production_candidate(
        &host.root,
        &host.bootstrap(),
        &store,
        &request.repository_id,
        &request.catalog_digest,
        &request.manifest_digest,
        &request.expected_active,
        &request.expected_revision,
        unix_timestamp(),
        |path, package, _candidate| {
            assert_eq!(path, cached);
            Ok(package.manifest.sequence)
        },
    )
    .unwrap();
    assert_eq!(sequence, 11);
}

#[test]
fn a_held_file_that_no_longer_matches_is_fetched_instead_of_reused() {
    let (host, update) = host_with_update();
    let running_directory = package_directory(&host.root, &host.running.digest).unwrap();
    fs::write(
        running_directory.join("amaterasu.display.so"),
        b"display librarX",
    )
    .unwrap();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    run(&mut service, &mut shell, Action::Check);
    host.connector.take_requests();

    let ready = run(&mut service, &mut shell, Action::Download);
    assert_eq!(ready["phase"], "ready");
    assert!(artifact_requests(&host.connector).contains(&"amaterasu.display.so".to_owned()));
    assert_eq!(
        fs::read(host.cached(&update).join("amaterasu.display.so")).unwrap(),
        b"display library"
    );
}

#[test]
fn a_cached_candidate_is_verified_again_instead_of_downloaded() {
    let (host, update) = host_with_update();
    let mut shell = ScriptedHost::accepting();
    let mut first = host.service();
    run(&mut first, &mut shell, Action::Check);
    assert_eq!(
        run(&mut first, &mut shell, Action::Download)["phase"],
        "ready"
    );
    drop(first);
    host.connector.take_requests();

    // A module instance started later finds the same verified candidate.
    let mut second = host.service();
    assert_eq!(
        ask(&mut second, &mut shell, Action::Status)["phase"],
        "idle"
    );
    run(&mut second, &mut shell, Action::Check);
    host.connector.take_requests();
    let ready = run(&mut second, &mut shell, Action::Download);
    assert_eq!(ready["phase"], "ready");
    assert_eq!(ready["transferred_bytes"], 0);
    assert!(host.connector.take_requests().is_empty());

    // Cached bytes that changed are refused, and never replaced or trusted.
    fs::write(host.cached(&update).join("amaterasu.stats.so"), b"changed").unwrap();
    let mut third = host.service();
    run(&mut third, &mut shell, Action::Check);
    let failed = run(&mut third, &mut shell, Action::Download);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "verification_failed");
    assert_eq!(
        refusal(&ask(&mut third, &mut shell, Action::Activate)),
        "not_ready"
    );
}

#[test]
fn a_tampered_manifest_is_refused_and_leaves_nothing_in_the_cache() {
    // Altered before the check: the catalog does not list these bytes.
    let (host, update) = host_with_update();
    let served = host.publisher.set_directory(&update, IMPLEMENTATION);
    let mut manifest: ReleaseManifest =
        serde_json::from_slice(&update.files["manifest.json"]).unwrap();
    manifest.sequence = 99;
    fs::write(
        served.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    let failed = run(&mut service, &mut shell, Action::Check);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "manifest_rejected");
    assert!(failed["update"].is_null());
    assert!(failed["checked_unix_ms"].is_null());
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Download)),
        "nothing_to_download"
    );
    assert!(host.cache_entries().is_empty());

    // Altered after an honest check: the download refuses the same bytes.
    let (host, update) = host_with_update();
    let mut service = host.service();
    assert_eq!(
        run(&mut service, &mut shell, Action::Check)["phase"],
        "available"
    );
    let served = host.publisher.set_directory(&update, IMPLEMENTATION);
    fs::write(
        served.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let failed = run(&mut service, &mut shell, Action::Download);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "verification_failed");
    assert!(host.cache_entries().is_empty());
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Activate)),
        "not_ready"
    );
    assert!(shell.requests.is_empty());

    // The listed manifest under a signature the approved key did not make.
    let (host, update) = host_with_update();
    let intruder = Publisher::new(host.root.join("intruder"), 72);
    let forged = ReleaseSignature {
        key_id: host.publisher.fingerprint(),
        signature: encode_hex(
            &intruder
                .signer
                .sign(&update.files["manifest.json"])
                .to_bytes(),
        ),
    };
    fs::write(
        host.publisher
            .set_directory(&update, IMPLEMENTATION)
            .join("manifest.sig"),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();
    let mut service = host.service();
    let failed = run(&mut service, &mut shell, Action::Check);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "manifest_rejected");
    assert!(host.cache_entries().is_empty());
}

#[test]
fn a_tampered_artifact_is_refused_and_leaves_nothing_in_the_cache() {
    let (host, update) = host_with_update();
    let served = host.publisher.set_directory(&update, IMPLEMENTATION);
    // The same length, so only its digest gives it away.
    fs::write(served.join("amaterasu.stats.so"), b"statistics librarY").unwrap();
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    assert_eq!(
        run(&mut service, &mut shell, Action::Check)["phase"],
        "available"
    );
    let failed = run(&mut service, &mut shell, Action::Download);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "verification_failed");
    assert_eq!(failed["update"]["digest"], update.digest.as_str());
    assert!(host.cache_entries().is_empty());
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Activate)),
        "not_ready"
    );
    assert!(shell.requests.is_empty());

    // Once the repository serves the signed bytes again, a retry succeeds.
    fs::write(
        served.join("amaterasu.stats.so"),
        &update.files["amaterasu.stats.so"],
    )
    .unwrap();
    assert_eq!(
        run(&mut service, &mut shell, Action::Download)["phase"],
        "ready"
    );
    assert_eq!(host.cache_entries(), std::slice::from_ref(&update.digest));
}

#[test]
fn an_artifact_the_repository_cannot_serve_fails_the_download() {
    let (host, update) = host_with_update();
    let served = host.publisher.set_directory(&update, IMPLEMENTATION);
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    run(&mut service, &mut shell, Action::Check);
    fs::remove_file(served.join("amaterasu.input.so")).unwrap();
    let failed = run(&mut service, &mut shell, Action::Download);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "download_failed");
    assert!(host.cache_entries().is_empty());

    // A longer file than the manifest signs is cut off by the transport.
    fs::write(
        served.join("amaterasu.input.so"),
        b"input library, second build and more",
    )
    .unwrap();
    let failed = run(&mut service, &mut shell, Action::Download);
    assert_eq!(failed["error"], "download_failed");
    assert!(host.cache_entries().is_empty());
}

#[test]
fn a_check_reports_why_the_repository_offers_nothing_usable() {
    let mut shell = ScriptedHost::accepting();
    let check = |host: &Host, shell: &mut ScriptedHost| {
        let mut service = host.service();
        run(&mut service, shell, Action::Check)
    };

    // Nothing is served at all.
    let host = Host::running(10, &first_release());
    let failed = check(&host, &mut shell);
    assert_eq!(failed["phase"], "failed");
    assert_eq!(failed["error"], "catalog_download_failed");

    // A catalog another key signed.
    let intruder = Publisher::new(host.publisher.directory.clone(), 72);
    intruder.publish_catalog(2, vec![host.running.listed()]);
    assert_eq!(check(&host, &mut shell)["error"], "catalog_rejected");

    // A different catalog at the sequence the host already accepted.
    host.publisher.publish(5, &[&host.running]);
    assert_eq!(check(&host, &mut shell)["phase"], "current");
    host.publisher.publish_catalog(5, Vec::new());
    assert_eq!(check(&host, &mut shell)["error"], "catalog_rejected");

    // A listed set whose manifest the repository does not serve.
    let update = host.publisher.sign_set(11, &second_release());
    host.publisher
        .publish_catalog(6, vec![host.running.listed(), update.listed()]);
    assert_eq!(
        check(&host, &mut shell)["error"],
        "manifest_download_failed"
    );

    // Two lines, and the running set on neither.
    let unrelated = Host::running(10, &first_release());
    let other = unrelated.publisher.sign_set(11, &second_release());
    unrelated.publisher.publish_catalog(
        2,
        vec![
            other.listed(),
            other.listed_as("amaterasu.rtx-hdr", "amaterasu-linux-session-rtx-hdr"),
        ],
    );
    assert_eq!(
        check(&unrelated, &mut shell)["error"],
        "candidate_ambiguous"
    );

    // The publisher was revoked after it supplied the running set.
    host.store().revoke(REPOSITORY).unwrap();
    assert_eq!(check(&host, &mut shell)["error"], "repository_unavailable");
}

#[test]
fn a_repository_with_nothing_newer_leaves_the_host_current() {
    let mut shell = ScriptedHost::accepting();
    let host = Host::running(10, &first_release());
    host.publisher.publish(2, &[&host.running]);
    let mut service = host.service();
    let current = run(&mut service, &mut shell, Action::Check);
    assert_eq!(current["phase"], "current");
    assert!(current["update"].is_null());
    assert!(current["error"].is_null());
    assert!(current["checked_unix_ms"].is_u64());
    // The running set's own manifest is not fetched again.
    assert_eq!(
        artifact_requests(&host.connector),
        ["catalog.json", "catalog.sig"]
    );
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Download)),
        "nothing_to_download"
    );

    // An older release on the line is not an update, listed or not.
    let older = host.publisher.sign_set(
        9,
        &[module(SLOT, "0.0.9", b"runtime library, earlier build")],
    );
    host.publisher.publish(3, &[&older]);
    assert_eq!(
        run(&mut service, &mut shell, Action::Check)["phase"],
        "current"
    );
    host.publisher.publish(4, &[&older, &host.running]);
    assert_eq!(
        run(&mut service, &mut shell, Action::Check)["phase"],
        "current"
    );
}

#[test]
fn a_repository_behind_the_host_is_current_and_not_a_rejected_catalog() {
    let mut shell = ScriptedHost::accepting();
    // A local import left the host holding catalog 7; the repository has
    // published as far as 5.
    let host = Host::running(10, &first_release());
    host.publisher.publish(7, &[&host.running]);
    let mut service = host.service();
    let current = run(&mut service, &mut shell, Action::Check);
    assert_eq!(current["phase"], "current");
    assert_eq!(current["repository_behind"], false);
    let accepted = || {
        host.store()
            .catalog(REPOSITORY, &current_target(), unix_timestamp())
            .unwrap()
            .sequence()
    };

    let update = host.publisher.sign_set(11, &second_release());
    host.publisher.publish(5, &[&host.running, &update]);
    host.connector.take_requests();
    let behind = run(&mut service, &mut shell, Action::Check);
    assert_eq!(behind["ok"], true);
    assert_eq!(behind["phase"], "current");
    assert_eq!(behind["repository_behind"], true);
    assert!(behind["error"].is_null());
    assert!(behind["checked_unix_ms"].is_u64());
    // Nothing a catalog the host cannot accept lists is offered or fetched.
    assert!(behind["update"].is_null());
    assert_eq!(
        artifact_requests(&host.connector),
        ["catalog.json", "catalog.sig"]
    );
    assert_eq!(accepted(), 7);
    assert_eq!(
        refusal(&ask(&mut service, &mut shell, Action::Download)),
        "nothing_to_download"
    );

    // An earlier sequence says nothing by itself. The catalog must be the
    // approved publisher's own, for this host's target.
    let rejected = |service: &mut TestService, shell: &mut ScriptedHost| {
        let failed = run(service, shell, Action::Check);
        assert_eq!(failed["phase"], "failed", "{failed}");
        assert_eq!(failed["error"], "catalog_rejected");
        assert_eq!(failed["repository_behind"], false);
    };
    let intruder = Publisher::new(host.publisher.directory.clone(), 72);
    intruder.publish_catalog(5, vec![host.running.listed()]);
    rejected(&mut service, &mut shell);
    host.publisher
        .publish_catalog_for("another-target", 5, vec![host.running.listed()]);
    rejected(&mut service, &mut shell);
    // A different catalog at the accepted sequence is a substitution.
    host.publisher.publish_catalog(7, Vec::new());
    rejected(&mut service, &mut shell);
    assert_eq!(accepted(), 7);

    // Once the repository has caught up, its offer is taken as usual.
    host.publisher.publish(8, &[&host.running, &update]);
    let available = run(&mut service, &mut shell, Action::Check);
    assert_eq!(available["phase"], "available");
    assert_eq!(available["repository_behind"], false);
    assert_eq!(available["update"]["digest"], update.digest.as_str());
    assert_eq!(accepted(), 8);
}

#[test]
fn the_update_follows_the_line_of_the_running_set() {
    // The repository offers the session set with and without RTX HDR.
    let host = Host::running(10, &first_release());
    let plain = host.publisher.sign_set(12, &second_release());
    let rtx = host.publisher.sign_set(
        13,
        &[
            module(SLOT, "0.2.1", b"runtime library, RTX build"),
            module("amaterasu.rtx-hdr", "0.2.1", b"RTX HDR library"),
        ],
    );
    host.publisher.serve(&plain, IMPLEMENTATION);
    host.publisher
        .serve(&rtx, "amaterasu-linux-session-rtx-hdr");
    host.publisher.publish_catalog(
        2,
        vec![
            host.running.listed(),
            plain.listed(),
            rtx.listed_as(SLOT, "amaterasu-linux-session-rtx-hdr"),
        ],
    );
    let mut service = host.service();
    let mut shell = ScriptedHost::accepting();
    let available = run(&mut service, &mut shell, Action::Check);
    assert_eq!(available["phase"], "available");
    assert_eq!(available["update"]["digest"], plain.digest.as_str());
    assert_eq!(
        available["update"]["implementation"],
        "amaterasu-linux-session"
    );
}
