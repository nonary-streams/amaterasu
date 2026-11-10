use super::*;
use crate::{
    ComponentManifest, ReleaseManifest, current_target, package_directory, random_identifier, web,
    write_json_atomically,
};
use amaterasu_contracts::{Grants, RuntimeConfig};
use ed25519_dalek::{Signer, SigningKey};
use std::fs;
use std::path::PathBuf;

struct MemoryTransport(BTreeMap<String, Vec<u8>>);

impl RepositoryTransport for MemoryTransport {
    fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>> {
        let bytes = self
            .0
            .get(relative)
            .ok_or("missing fixture response")?
            .clone();
        if bytes.len() as u64 > maximum {
            return Err("fixture response exceeds byte limit".into());
        }
        Ok(bytes)
    }
}

struct Fixture {
    root: PathBuf,
    signer: SigningKey,
    approved: ApprovedRepository,
    bootstrap: BootstrapConfig,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-repository-test-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let signer = SigningKey::from_bytes(&[61; 32]);
        let public_key = encode_hex(signer.verifying_key().as_bytes());
        let fingerprint = sha256(signer.verifying_key().as_bytes());
        let approved = ApprovedRepository::approve(
            "test-publisher".into(),
            "https://updates.example.test/releases".into(),
            public_key,
            fingerprint,
        )
        .unwrap();
        let bootstrap = BootstrapConfig {
            development: true,
            allow_unsigned: true,
            trusted_keys: BTreeMap::new(),
            policy: RuntimeConfig::default(),
        };
        write_json_atomically(&root.join("bootstrap.json"), &bootstrap).unwrap();
        Self {
            root,
            signer,
            approved,
            bootstrap,
        }
    }

    fn web_bundle(&self) -> (PathBuf, Candidate) {
        let source = self.root.join("web-source");
        let output = self.root.join("web-package");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("index.html"), "<h1>fixture</h1>").unwrap();
        let manifest = web::prepare_bundle(&source, &output, 3, 500).unwrap();
        let raw = serde_json::to_vec(&manifest).unwrap();
        self.write_signed_manifest(&output, &raw);
        (
            output,
            Candidate {
                slot: "amaterasu.ui".into(),
                implementation: "fixture-ui".into(),
                version: "3".into(),
                kind: CandidateKind::WebBundle,
                manifest_sha256: sha256(&raw),
            },
        )
    }

    fn native_set(&self) -> (PathBuf, Candidate) {
        self.native_set_variant(
            "native-package",
            4,
            "1.0",
            b"synthetic library bytes, never loaded",
        )
    }

    fn native_set_variant(
        &self,
        name: &str,
        sequence: u64,
        version: &str,
        artifact: &[u8],
    ) -> (PathBuf, Candidate) {
        let output = self.root.join(name);
        fs::create_dir(&output).unwrap();
        fs::write(output.join("runtime.bin"), artifact).unwrap();
        fs::write(output.join("capture.bin"), artifact).unwrap();
        let component = |id: &str, file: &str, runtime| ComponentManifest {
            id: id.into(),
            version: version.into(),
            file: file.into(),
            sha256: sha256(artifact),
            size: artifact.len() as u64,
            abi_major: amaterasu_abi::ABI_MAJOR,
            runtime,
            requires: BTreeMap::new(),
            requested: Grants::default(),
        };
        let manifest = ReleaseManifest {
            privileged_requests: Vec::new(),
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            sequence,
            expires_unix: 500,
            target: current_target(),
            components: vec![
                component("amaterasu.runtime", "runtime.bin", true),
                component("amaterasu.capture", "capture.bin", false),
            ],
        };
        let raw = serde_json::to_vec(&manifest).unwrap();
        self.write_signed_manifest(&output, &raw);
        (
            output,
            Candidate {
                slot: "amaterasu.capture".into(),
                implementation: "fixture-capture".into(),
                version: version.into(),
                kind: CandidateKind::NativeSet,
                manifest_sha256: sha256(&raw),
            },
        )
    }

    fn write_signed_manifest(&self, directory: &Path, raw: &[u8]) {
        fs::write(directory.join("manifest.json"), raw).unwrap();
        let signature = ReleaseSignature {
            key_id: self.approved.fingerprint.clone(),
            signature: encode_hex(&self.signer.sign(raw).to_bytes()),
        };
        write_json_atomically(&directory.join("manifest.sig"), &signature).unwrap();
    }

    fn signed_catalog(&self, candidates: Vec<Candidate>, sequence: u64) -> (Vec<u8>, String) {
        let catalog = Catalog {
            schema: 1,
            repository: self.approved.id.clone(),
            sequence,
            expires_unix: 500,
            target: current_target(),
            candidates,
        };
        let raw = serde_json::to_vec(&catalog).unwrap();
        let mut signed = CATALOG_DOMAIN.to_vec();
        signed.extend_from_slice(&raw);
        let signature = encode_hex(&self.signer.sign(&signed).to_bytes());
        (raw, signature)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn local_import_verifies_offline_payload_without_publication_or_selection_changes() {
    let fixture = Fixture::new();
    let (package, candidate) = fixture.native_set();
    let bundle = fixture.root.join("offline");
    let payload = bundle.join("native/fixture-capture/1.0");
    fs::create_dir_all(&payload).unwrap();
    for entry in fs::read_dir(package).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), payload.join(entry.file_name())).unwrap();
    }
    let (raw, signature) = fixture.signed_catalog(vec![candidate.clone()], 7);
    fs::write(bundle.join("catalog.json"), &raw).unwrap();
    fs::write(bundle.join("catalog.sig"), &signature).unwrap();
    let store = RepositoryStore::new(fixture.root.join("repositories")).unwrap();
    store.approve(fixture.approved.clone()).unwrap();
    // An approval alone cannot extend the installed bootstrap's trust.
    assert!(
        import_local_native(
            &fixture.root,
            &fixture.approved.id,
            &bundle,
            &candidate.manifest_sha256,
            100
        )
        .is_err()
    );
    let mut bootstrap = fixture.bootstrap.clone();
    bootstrap.development = false;
    bootstrap.allow_unsigned = false;
    bootstrap.trusted_keys = fixture.approved.keys();
    write_json_atomically(&fixture.root.join("bootstrap.json"), &bootstrap).unwrap();
    let original = fs::read(fixture.root.join("bootstrap.json")).unwrap();
    // A corrupt artifact must not advance the catalog, and retry must work.
    let artifact = fs::read(payload.join("capture.bin")).unwrap();
    fs::write(payload.join("capture.bin"), b"corrupt").unwrap();
    assert!(
        import_local_native(
            &fixture.root,
            &fixture.approved.id,
            &bundle,
            &candidate.manifest_sha256,
            100
        )
        .is_err()
    );
    assert!(
        store
            .catalog(&fixture.approved.id, &current_target(), 100)
            .is_err()
    );
    fs::write(payload.join("capture.bin"), artifact).unwrap();
    let digest = import_local_native(
        &fixture.root,
        &fixture.approved.id,
        &bundle,
        &candidate.manifest_sha256,
        100,
    )
    .unwrap();
    assert_eq!(digest, sha256(&raw));
    assert_eq!(
        original,
        fs::read(fixture.root.join("bootstrap.json")).unwrap()
    );
    assert!(!fixture.root.join("selection.json").exists());
    // Idempotent import is allowed; a lower signed catalog remains rejected.
    import_local_native(
        &fixture.root,
        &fixture.approved.id,
        &bundle,
        &candidate.manifest_sha256,
        100,
    )
    .unwrap();
    let (older, signature) = fixture.signed_catalog(vec![candidate.clone()], 6);
    fs::write(bundle.join("catalog.json"), older).unwrap();
    fs::write(bundle.join("catalog.sig"), signature).unwrap();
    assert!(
        import_local_native(
            &fixture.root,
            &fixture.approved.id,
            &bundle,
            &candidate.manifest_sha256,
            100
        )
        .is_err()
    );
    assert_eq!(
        store
            .catalog(&fixture.approved.id, &current_target(), 100)
            .unwrap()
            .digest(),
        digest
    );
}

#[test]
fn approval_requires_an_independently_confirmed_fingerprint() {
    let fixture = Fixture::new();
    assert!(
        ApprovedRepository::approve(
            "test".into(),
            fixture.approved.location.clone(),
            fixture.approved.public_key.clone(),
            "0".repeat(64),
        )
        .is_err()
    );
    assert!(
        ApprovedRepository::approve(
            "test".into(),
            "http://updates.example.test".into(),
            fixture.approved.public_key.clone(),
            fixture.approved.fingerprint.clone(),
        )
        .is_err()
    );
    for invalid in ["..", "a/b", "../publisher"] {
        assert!(
            ApprovedRepository::approve(
                invalid.into(),
                fixture.approved.location.clone(),
                fixture.approved.public_key.clone(),
                fixture.approved.fingerprint.clone(),
            )
            .is_err()
        );
    }
}

#[test]
fn catalog_lists_ui_and_native_slots_and_authenticates_both_candidates() {
    let fixture = Fixture::new();
    let (web_directory, web_candidate) = fixture.web_bundle();
    let (native_directory, native_candidate) = fixture.native_set();
    let (raw, signature) = fixture.signed_catalog(vec![web_candidate, native_candidate], 7);
    let verified = verify_catalog(
        &fixture.approved,
        &raw,
        &signature,
        &CatalogCheckpoint::default(),
        &current_target(),
        100,
    )
    .unwrap();
    assert_eq!(verified.candidates().len(), 2);
    for (candidate, directory) in verified
        .candidates()
        .iter()
        .zip([&web_directory, &native_directory])
    {
        verify_candidate(
            &fixture.approved,
            &verified,
            candidate,
            directory,
            &fixture.bootstrap,
            100,
        )
        .unwrap();
    }
    fs::write(web_directory.join("index.html"), "tampered").unwrap();
    assert!(
        verify_candidate(
            &fixture.approved,
            &verified,
            &verified.candidates()[0],
            &web_directory,
            &fixture.bootstrap,
            100,
        )
        .is_err()
    );
    fs::write(native_directory.join("capture.bin"), "tampered").unwrap();
    assert!(
        verify_candidate(
            &fixture.approved,
            &verified,
            &verified.candidates()[1],
            &native_directory,
            &fixture.bootstrap,
            100,
        )
        .is_err()
    );
}

#[test]
fn catalog_rejects_replay_substitution_bad_signature_and_noncanonical_json() {
    let fixture = Fixture::new();
    let (_, candidate) = fixture.web_bundle();
    let (raw, signature) = fixture.signed_catalog(vec![candidate], 7);
    let verified = verify_catalog(
        &fixture.approved,
        &raw,
        &signature,
        &CatalogCheckpoint::default(),
        &current_target(),
        100,
    )
    .unwrap();
    let (older, older_signature) = fixture.signed_catalog(vec![], 6);
    assert!(
        verify_catalog(
            &fixture.approved,
            &older,
            &older_signature,
            verified.next_checkpoint(),
            &current_target(),
            100,
        )
        .is_err()
    );
    let (substitute, substitute_signature) = fixture.signed_catalog(vec![], 7);
    assert!(
        verify_catalog(
            &fixture.approved,
            &substitute,
            &substitute_signature,
            verified.next_checkpoint(),
            &current_target(),
            100,
        )
        .is_err()
    );
    assert!(
        verify_catalog(
            &fixture.approved,
            &raw,
            &"0".repeat(128),
            &CatalogCheckpoint::default(),
            &current_target(),
            100,
        )
        .is_err()
    );
    let pretty = serde_json::to_vec_pretty(&verified.catalog).unwrap();
    assert!(
        verify_catalog(
            &fixture.approved,
            &pretty,
            &signature,
            &CatalogCheckpoint::default(),
            &current_target(),
            100,
        )
        .is_err()
    );
}

#[test]
fn candidate_rejects_wrong_publisher_unlisted_choice_and_expired_catalog() {
    let fixture = Fixture::new();
    let (directory, candidate) = fixture.web_bundle();
    let (raw, signature) = fixture.signed_catalog(vec![candidate], 1);
    let verified = verify_catalog(
        &fixture.approved,
        &raw,
        &signature,
        &CatalogCheckpoint::default(),
        &current_target(),
        100,
    )
    .unwrap();
    let other_signer = SigningKey::from_bytes(&[62; 32]);
    let other = ApprovedRepository::approve(
        fixture.approved.id.clone(),
        fixture.approved.location.clone(),
        encode_hex(other_signer.verifying_key().as_bytes()),
        sha256(other_signer.verifying_key().as_bytes()),
    )
    .unwrap();
    assert!(
        verify_candidate(
            &other,
            &verified,
            &verified.candidates()[0],
            &directory,
            &fixture.bootstrap,
            100,
        )
        .is_err()
    );
    let mut unlisted = verified.candidates()[0].clone();
    unlisted.implementation = "other-ui".into();
    assert!(
        verify_candidate(
            &fixture.approved,
            &verified,
            &unlisted,
            &directory,
            &fixture.bootstrap,
            100,
        )
        .is_err()
    );
    assert!(
        verify_candidate(
            &fixture.approved,
            &verified,
            &verified.candidates()[0],
            &directory,
            &fixture.bootstrap,
            500,
        )
        .is_err()
    );
}

#[test]
fn catalog_bound_manifest_must_use_canonical_json() {
    let fixture = Fixture::new();
    let (directory, mut candidate) = fixture.web_bundle();
    let manifest: web::Manifest = crate::read_json(&directory.join("manifest.json")).unwrap();
    let pretty = serde_json::to_vec_pretty(&manifest).unwrap();
    fixture.write_signed_manifest(&directory, &pretty);
    candidate.manifest_sha256 = sha256(&pretty);
    let (raw, signature) = fixture.signed_catalog(vec![candidate], 1);
    let catalog = verify_catalog(
        &fixture.approved,
        &raw,
        &signature,
        &CatalogCheckpoint::default(),
        &current_target(),
        100,
    )
    .unwrap();
    assert!(
        verify_candidate(
            &fixture.approved,
            &catalog,
            &catalog.candidates()[0],
            &directory,
            &fixture.bootstrap,
            100,
        )
        .is_err()
    );
}

#[test]
fn approved_store_persists_catalog_checkpoint_and_revocation() {
    let fixture = Fixture::new();
    let store = RepositoryStore::new(fixture.root.join("approvals")).unwrap();
    store.approve(fixture.approved.clone()).unwrap();
    assert!(store.approve(fixture.approved.clone()).is_err());
    let (_, candidate) = fixture.web_bundle();
    let (raw, signature) = fixture.signed_catalog(vec![candidate], 7);
    store
        .accept_catalog(
            &fixture.approved.id,
            &raw,
            &signature,
            &current_target(),
            100,
        )
        .unwrap();
    let reopened = RepositoryStore::new(fixture.root.join("approvals")).unwrap();
    assert_eq!(
        reopened.list(&current_target(), 100).unwrap()[0]
            .candidates
            .len(),
        1
    );
    let (older, older_signature) = fixture.signed_catalog(vec![], 6);
    assert!(
        reopened
            .accept_catalog(
                &fixture.approved.id,
                &older,
                &older_signature,
                &current_target(),
                100,
            )
            .is_err()
    );
    reopened.revoke(&fixture.approved.id).unwrap();
    assert!(reopened.approved(&fixture.approved.id).is_err());
    assert!(reopened.active_keys().unwrap().is_empty());
    assert!(reopened.list(&current_target(), 100).unwrap()[0].revoked);
    assert!(reopened.approve(fixture.approved.clone()).is_err());
}

#[test]
fn listing_reports_each_catalog_without_hiding_other_repositories() {
    let fixture = Fixture::new();
    let store = RepositoryStore::new(fixture.root.join("approvals")).unwrap();
    store.approve(fixture.approved.clone()).unwrap();
    let second_signer = SigningKey::from_bytes(&[9; 32]);
    let second_key = encode_hex(second_signer.verifying_key().as_bytes());
    let second = ApprovedRepository::approve(
        "second-publisher".into(),
        "https://second.example.test/repository".into(),
        second_key.clone(),
        sha256(second_signer.verifying_key().as_bytes()),
    )
    .unwrap();
    store.approve(second).unwrap();
    let listed = |now| store.list(&current_target(), now).unwrap();
    assert!(
        listed(100)
            .iter()
            .all(|repository| repository.catalog == CatalogListing::Missing)
    );

    let (_, candidate) = fixture.web_bundle();
    let (raw, signature) = fixture.signed_catalog(vec![candidate], 7);
    store
        .accept_catalog(
            &fixture.approved.id,
            &raw,
            &signature,
            &current_target(),
            100,
        )
        .unwrap();
    let current = listed(100);
    let accepted = current
        .iter()
        .find(|repository| repository.id == fixture.approved.id)
        .unwrap();
    assert_eq!(
        accepted.catalog,
        CatalogListing::Current {
            sequence: "7".into(),
            expires_unix: 500
        }
    );
    assert_eq!(accepted.candidates.len(), 1);

    // An expired catalog withholds its own choices and nothing else.
    let lapsed = listed(500);
    assert_eq!(lapsed.len(), 2);
    let expired = lapsed
        .iter()
        .find(|repository| repository.id == fixture.approved.id)
        .unwrap();
    assert_eq!(
        expired.catalog,
        CatalogListing::Expired { expires_unix: 500 }
    );
    assert!(expired.candidates.is_empty());

    // A wrong target is not an expiry, before or after the expiry time.
    for now in [100, 500] {
        let other_target = store.list("other-target", now).unwrap();
        let unavailable = other_target
            .iter()
            .find(|repository| repository.id == fixture.approved.id)
            .unwrap();
        assert_eq!(unavailable.catalog, CatalogListing::Unavailable);
        assert!(unavailable.candidates.is_empty());
    }

    store.revoke(&fixture.approved.id).unwrap();
    let revoked = listed(100);
    let removed = revoked
        .iter()
        .find(|repository| repository.id == fixture.approved.id)
        .unwrap();
    assert_eq!(removed.catalog, CatalogListing::Missing);
    assert!(removed.candidates.is_empty());
}

#[test]
fn approval_rejection_names_the_unacceptable_field() {
    let fixture = Fixture::new();
    let approved = &fixture.approved;
    let check = |id: &str, location: &str, key: &str, fingerprint: &str| {
        ApprovedRepository::check(id.into(), location.into(), key.into(), fingerprint.into())
            .map(|_| ())
    };
    assert_eq!(
        check(
            &approved.id,
            &approved.location,
            &approved.public_key,
            &approved.fingerprint
        ),
        Ok(())
    );
    assert_eq!(
        check(
            "Not Valid",
            &approved.location,
            &approved.public_key,
            &approved.fingerprint
        ),
        Err(ApprovalRejection::Identity)
    );
    assert_eq!(
        check(
            &approved.id,
            "http://plain.example.test",
            &approved.public_key,
            &approved.fingerprint
        ),
        Err(ApprovalRejection::Location)
    );
    assert_eq!(
        check(
            &approved.id,
            &approved.location,
            "not-a-key",
            &approved.fingerprint
        ),
        Err(ApprovalRejection::PublicKey)
    );
    assert_eq!(
        check(
            &approved.id,
            &approved.location,
            &approved.public_key,
            &"0".repeat(64)
        ),
        Err(ApprovalRejection::Fingerprint)
    );
}

#[test]
fn selection_is_observable_while_the_activation_lock_is_held() {
    let fixture = Fixture::new();
    let digest = "a".repeat(64);
    let selection = ActivationState {
        active: Some(digest.clone()),
        highest_sequence: 4,
        repository_sets: BTreeMap::from([(
            digest.clone(),
            RepositorySet {
                repository_id: fixture.approved.id.clone(),
                key_fingerprint: fixture.approved.fingerprint.clone(),
            },
        )]),
        ..Default::default()
    };
    write_json_atomically(&fixture.root.join("selection.json"), &selection).unwrap();
    let service_lock = RootLock::acquire(&fixture.root).unwrap();
    assert!(read_native_selection(&fixture.root).is_err());

    let observed = observe_native_selection(&fixture.root).unwrap();
    assert_eq!(observed.selection.active.as_deref(), Some(digest.as_str()));
    assert_eq!(observed.selection.highest_sequence, 4);
    assert_eq!(
        observed.repository_sets[&digest].repository_id,
        fixture.approved.id
    );
    drop(service_lock);
    assert_eq!(
        read_native_selection(&fixture.root).unwrap().revision,
        observed.selection.revision
    );
}

#[test]
fn selection_authorization_excludes_concurrent_revocation() {
    let fixture = Fixture::new();
    let store = RepositoryStore::new(fixture.root.join("selection-approvals")).unwrap();
    store
        .with_active_keys(|keys| {
            assert!(keys.is_empty());
            Ok(())
        })
        .unwrap();
    store.approve(fixture.approved.clone()).unwrap();
    store
        .with_active_keys(|keys| {
            assert!(keys.contains_key(&fixture.approved.fingerprint));
            assert!(store.revoke(&fixture.approved.id).is_err());
            assert!(keys.contains_key(&fixture.approved.fingerprint));
            Ok(())
        })
        .unwrap();
    store.revoke(&fixture.approved.id).unwrap();
    assert!(store.active_keys().unwrap().is_empty());
}

#[test]
fn native_staging_records_publisher_and_replay_floor_without_loading_code() {
    let fixture = Fixture::new();
    let (source, candidate) = fixture.native_set();
    let (raw, signature) = fixture.signed_catalog(vec![candidate.clone()], 7);
    let store = RepositoryStore::new(fixture.root.join("approvals-native")).unwrap();
    store.approve(fixture.approved.clone()).unwrap();
    store
        .accept_catalog(
            &fixture.approved.id,
            &raw,
            &signature,
            &current_target(),
            100,
        )
        .unwrap();
    write_json_atomically(
        &fixture.root.join("selection.json"),
        &ActivationState::default(),
    )
    .unwrap();
    let catalog = store
        .catalog(&fixture.approved.id, &current_target(), 100)
        .unwrap();
    let digest = store
        .with_current_catalog(
            &fixture.approved.id,
            catalog.digest(),
            &current_target(),
            100,
            |approved, current| {
                stage_native_candidate(
                    &source,
                    &fixture.root,
                    &fixture.bootstrap,
                    approved,
                    current,
                    &candidate,
                    100,
                )
            },
        )
        .unwrap();
    let mut state: ActivationState = read_json(&fixture.root.join("selection.json")).unwrap();
    assert!(state.pending.is_none());
    assert_eq!(
        state.repository_sets[&digest].key_fingerprint,
        fixture.approved.fingerprint
    );
    assert_eq!(
        state.repository_floors[&fixture.approved.fingerprint].sequence,
        4
    );
    assert!(package_directory(&fixture.root, &digest).unwrap().exists());
    activate_staged_native(&fixture.root, &fixture.bootstrap, &store, &digest, 100).unwrap();
    state = read_json(&fixture.root.join("selection.json")).unwrap();
    assert_eq!(state.pending.as_deref(), Some(digest.as_str()));
    state
        .repository_floors
        .get_mut(&fixture.approved.fingerprint)
        .unwrap()
        .sequence = 5;
    write_json_atomically(&fixture.root.join("selection.json"), &state).unwrap();
    assert!(
        activate_staged_native(&fixture.root, &fixture.bootstrap, &store, &digest, 100).is_err()
    );
    assert!(
        store
            .with_current_catalog(
                &fixture.approved.id,
                catalog.digest(),
                &current_target(),
                100,
                |approved, current| stage_native_candidate(
                    &source,
                    &fixture.root,
                    &fixture.bootstrap,
                    approved,
                    current,
                    &candidate,
                    100,
                ),
            )
            .is_err()
    );
    let (withdrawn, withdrawal_signature) = fixture.signed_catalog(vec![], 8);
    store
        .accept_catalog(
            &fixture.approved.id,
            &withdrawn,
            &withdrawal_signature,
            &current_target(),
            100,
        )
        .unwrap();
    assert!(
        activate_staged_native(&fixture.root, &fixture.bootstrap, &store, &digest, 100).is_err()
    );
}

fn cache_candidate(
    store: &RepositoryStore,
    repository_id: &str,
    candidate: &Candidate,
    source: &PathBuf,
) -> PathBuf {
    let cached = store
        .native_candidate_cache_path(repository_id, &candidate.manifest_sha256)
        .unwrap();
    fs::create_dir(&cached).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_type().unwrap().is_file());
        fs::copy(entry.path(), cached.join(entry.file_name())).unwrap();
    }
    cached
}

#[test]
fn cached_native_stage_and_trial_authorization_are_exact_and_revision_guarded() {
    let fixture = Fixture::new();
    let (source, candidate) = fixture.native_set();
    let (raw, signature) = fixture.signed_catalog(vec![candidate.clone()], 7);
    let store = RepositoryStore::new(fixture.root.join("approvals-native-cas")).unwrap();
    store.approve(fixture.approved.clone()).unwrap();
    store
        .accept_catalog(
            &fixture.approved.id,
            &raw,
            &signature,
            &current_target(),
            100,
        )
        .unwrap();
    let catalog = store
        .catalog(&fixture.approved.id, &current_target(), 100)
        .unwrap();
    cache_candidate(&store, &fixture.approved.id, &candidate, &source);
    write_json_atomically(
        &fixture.root.join("selection.json"),
        &ActivationState::default(),
    )
    .unwrap();

    let before = read_native_selection(&fixture.root).unwrap();
    let mut production_bootstrap = fixture.bootstrap.clone();
    production_bootstrap.development = false;
    write_json_atomically(&fixture.root.join("bootstrap.json"), &production_bootstrap).unwrap();
    assert!(
        stage_cached_native_candidate(
            &fixture.root,
            &fixture.bootstrap,
            &store,
            &fixture.approved.id,
            catalog.digest(),
            &candidate,
            &before.revision,
            100,
        )
        .is_err()
    );
    assert!(
        !fixture
            .root
            .join("sets")
            .join(&candidate.manifest_sha256)
            .exists()
    );
    write_json_atomically(&fixture.root.join("bootstrap.json"), &fixture.bootstrap).unwrap();

    let staged = stage_cached_native_candidate(
        &fixture.root,
        &fixture.bootstrap,
        &store,
        &fixture.approved.id,
        catalog.digest(),
        &candidate,
        &before.revision,
        100,
    )
    .unwrap();
    assert_ne!(staged.revision, before.revision);
    assert!(staged.active.is_none());
    assert!(staged.pending.is_none());
    assert!(
        package_directory(&fixture.root, &candidate.manifest_sha256)
            .unwrap()
            .is_dir()
    );

    let stale_stage = stage_cached_native_candidate(
        &fixture.root,
        &fixture.bootstrap,
        &store,
        &fixture.approved.id,
        catalog.digest(),
        &candidate,
        &before.revision,
        100,
    )
    .unwrap_err();
    assert!(stale_stage.is_conflict());
    assert_eq!(
        read_native_selection(&fixture.root).unwrap().revision,
        staged.revision
    );

    let mut changed_bootstrap = fixture.bootstrap.clone();
    changed_bootstrap.policy.settings.insert(
        "amaterasu.test-change".into(),
        serde_json::json!({"changed": true}),
    );
    write_json_atomically(&fixture.root.join("bootstrap.json"), &changed_bootstrap).unwrap();
    assert!(
        authorize_staged_native_trial(
            &fixture.root,
            &fixture.bootstrap,
            &store,
            &fixture.approved.id,
            catalog.digest(),
            &candidate.manifest_sha256,
            &staged.revision,
            100,
        )
        .is_err()
    );
    assert_eq!(
        read_native_selection(&fixture.root).unwrap().revision,
        staged.revision
    );
    write_json_atomically(&fixture.root.join("bootstrap.json"), &fixture.bootstrap).unwrap();

    let pending = authorize_staged_native_trial(
        &fixture.root,
        &fixture.bootstrap,
        &store,
        &fixture.approved.id,
        catalog.digest(),
        &candidate.manifest_sha256,
        &staged.revision,
        100,
    )
    .unwrap();
    assert_eq!(
        pending.pending.as_deref(),
        Some(candidate.manifest_sha256.as_str())
    );
    assert!(pending.active.is_none());
    assert!(
        authorize_staged_native_trial(
            &fixture.root,
            &fixture.bootstrap,
            &store,
            &fixture.approved.id,
            catalog.digest(),
            &candidate.manifest_sha256,
            &staged.revision,
            100,
        )
        .unwrap_err()
        .is_conflict()
    );
}

#[cfg(unix)]
#[test]
fn dangling_selection_symlink_is_not_treated_as_an_empty_selection() {
    let fixture = Fixture::new();
    let selection = fixture.root.join("selection.json");
    std::os::unix::fs::symlink(fixture.root.join("missing-selection.json"), &selection).unwrap();

    assert!(read_native_selection(&fixture.root).is_err());
}

#[test]
fn previous_healthy_native_restore_is_trusted_cas_and_keeps_release_floor() {
    let fixture = Fixture::new();
    let (first_source, first_candidate) = fixture.native_set();
    let (second_source, second_candidate) = fixture.native_set_variant(
        "newer-native-package",
        5,
        "1.1",
        b"different synthetic library bytes, never loaded",
    );
    let (raw, signature) =
        fixture.signed_catalog(vec![first_candidate.clone(), second_candidate.clone()], 7);
    let store = RepositoryStore::new(fixture.root.join("approvals-native-restore")).unwrap();
    store.approve(fixture.approved.clone()).unwrap();
    store
        .accept_catalog(
            &fixture.approved.id,
            &raw,
            &signature,
            &current_target(),
            100,
        )
        .unwrap();
    let catalog = store
        .catalog(&fixture.approved.id, &current_target(), 100)
        .unwrap();
    cache_candidate(
        &store,
        &fixture.approved.id,
        &first_candidate,
        &first_source,
    );
    cache_candidate(
        &store,
        &fixture.approved.id,
        &second_candidate,
        &second_source,
    );
    let initial = read_native_selection(&fixture.root).unwrap();
    let staged_first = stage_cached_native_candidate(
        &fixture.root,
        &fixture.bootstrap,
        &store,
        &fixture.approved.id,
        catalog.digest(),
        &first_candidate,
        &initial.revision,
        100,
    )
    .unwrap();
    let staged_second = stage_cached_native_candidate(
        &fixture.root,
        &fixture.bootstrap,
        &store,
        &fixture.approved.id,
        catalog.digest(),
        &second_candidate,
        &staged_first.revision,
        100,
    )
    .unwrap();

    let selection_path = fixture.root.join("selection.json");
    let mut selection: ActivationState = read_json(&selection_path).unwrap();
    selection.active = Some(second_candidate.manifest_sha256.clone());
    selection.previous = Some(first_candidate.manifest_sha256.clone());
    selection.pending = Some(second_candidate.manifest_sha256.clone());
    selection
        .healthy_sets
        .insert(first_candidate.manifest_sha256.clone());
    selection
        .healthy_sets
        .insert(second_candidate.manifest_sha256.clone());
    let retained_floor = selection.repository_floors[&fixture.approved.fingerprint].clone();
    write_json_atomically(&selection_path, &selection).unwrap();

    let before_restore = read_native_selection(&fixture.root).unwrap();
    let mut changed_bootstrap = fixture.bootstrap.clone();
    changed_bootstrap.policy.settings.insert(
        "amaterasu.test-change".into(),
        serde_json::json!({"changed": true}),
    );
    write_json_atomically(&fixture.root.join("bootstrap.json"), &changed_bootstrap).unwrap();
    assert!(
        restore_previous_healthy_native(
            &fixture.root,
            &fixture.bootstrap,
            &store,
            Some(&second_candidate.manifest_sha256),
            &before_restore.revision,
            100,
        )
        .is_err()
    );
    assert_eq!(
        read_native_selection(&fixture.root).unwrap().revision,
        before_restore.revision
    );
    write_json_atomically(&fixture.root.join("bootstrap.json"), &fixture.bootstrap).unwrap();

    let restored = restore_previous_healthy_native(
        &fixture.root,
        &fixture.bootstrap,
        &store,
        Some(&second_candidate.manifest_sha256),
        &before_restore.revision,
        100,
    )
    .unwrap();
    assert_eq!(
        restored.active.as_deref(),
        Some(first_candidate.manifest_sha256.as_str())
    );
    assert_eq!(
        restored.previous.as_deref(),
        Some(second_candidate.manifest_sha256.as_str())
    );
    assert!(restored.pending.is_none());
    assert!(restored.trial.is_none());
    let after_restore: ActivationState = read_json(&selection_path).unwrap();
    assert_eq!(
        after_restore.repository_floors[&fixture.approved.fingerprint].sequence,
        retained_floor.sequence
    );
    assert_eq!(
        after_restore.repository_floors[&fixture.approved.fingerprint].digest,
        retained_floor.digest
    );
    assert_eq!(after_restore.highest_sequence, selection.highest_sequence);

    assert!(
        restore_previous_healthy_native(
            &fixture.root,
            &fixture.bootstrap,
            &store,
            Some(&second_candidate.manifest_sha256),
            &before_restore.revision,
            100,
        )
        .unwrap_err()
        .is_conflict()
    );
    let _ = staged_second;
}

#[test]
fn bounded_transport_refreshes_catalog_and_downloads_verified_web_bundle() {
    let fixture = Fixture::new();
    let (bundle, candidate) = fixture.web_bundle();
    let (raw, signature) = fixture.signed_catalog(vec![candidate], 9);
    let prefix = "web/fixture-ui/3";
    let mut responses = BTreeMap::from([
        ("catalog.json".into(), raw),
        ("catalog.sig".into(), signature.into_bytes()),
    ]);
    for name in ["manifest.json", "manifest.sig", "index.html"] {
        responses.insert(
            format!("{prefix}/{name}"),
            fs::read(bundle.join(name)).unwrap(),
        );
    }
    let transport = MemoryTransport(responses);
    let store = RepositoryStore::new(fixture.root.join("transport-approvals")).unwrap();
    store.approve(fixture.approved.clone()).unwrap();
    refresh_catalog(
        &store,
        &fixture.approved.id,
        &transport,
        &current_target(),
        100,
    )
    .unwrap();
    let catalog = store
        .catalog(&fixture.approved.id, &current_target(), 100)
        .unwrap();
    let output = fixture.root.join("downloaded-web");
    download_candidate(
        &transport,
        &fixture.approved,
        &catalog,
        &catalog.candidates()[0],
        &output,
        &fixture.bootstrap,
        100,
    )
    .unwrap();
    assert_eq!(
        fs::read(output.join("index.html")).unwrap(),
        b"<h1>fixture</h1>"
    );
    assert!(
        download_candidate(
            &transport,
            &fixture.approved,
            &catalog,
            &catalog.candidates()[0],
            &output,
            &fixture.bootstrap,
            100,
        )
        .is_err()
    );
}

#[test]
fn production_candidate_verification_is_read_only_and_checks_live_authority() {
    let fixture = Fixture::new();
    let (active_source, active_candidate) =
        fixture.native_set_variant("active-package", 3, "1.0", b"old trusted library");
    let (source, candidate) = fixture.native_set();
    let (raw, signature) = fixture.signed_catalog(vec![candidate.clone()], 7);
    let store = RepositoryStore::new(fixture.root.join("production-approvals")).unwrap();
    store.approve(fixture.approved.clone()).unwrap();
    store
        .accept_catalog(
            &fixture.approved.id,
            &raw,
            &signature,
            &current_target(),
            100,
        )
        .unwrap();
    let catalog = store
        .catalog(&fixture.approved.id, &current_target(), 100)
        .unwrap();
    let cache = cache_candidate(&store, &fixture.approved.id, &candidate, &source);
    let active_path = package_directory(&fixture.root, &active_candidate.manifest_sha256).unwrap();
    fs::create_dir_all(&active_path).unwrap();
    for entry in fs::read_dir(active_source).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), active_path.join(entry.file_name())).unwrap();
    }
    let mut bootstrap = fixture.bootstrap.clone();
    bootstrap.development = false;
    bootstrap.allow_unsigned = false;
    bootstrap.trusted_keys.insert(
        fixture.approved.fingerprint.clone(),
        fixture.approved.public_key.clone(),
    );
    write_json_atomically(&fixture.root.join("bootstrap.json"), &bootstrap).unwrap();
    let state = ActivationState {
        active: Some(active_candidate.manifest_sha256.clone()),
        highest_sequence: 3,
        ..Default::default()
    };
    write_json_atomically(&fixture.root.join("selection.json"), &state).unwrap();
    let revision = read_native_selection(&fixture.root).unwrap().revision;
    let selection_before = fs::read(fixture.root.join("selection.json")).unwrap();
    let _root_lock = RootLock::acquire(&fixture.root).unwrap();
    let verify = |expected_revision: &str| {
        with_verified_production_candidate(
            &fixture.root,
            &bootstrap,
            &store,
            &fixture.approved.id,
            catalog.digest(),
            &candidate.manifest_sha256,
            &active_candidate.manifest_sha256,
            expected_revision,
            100,
            |path, package, listed| {
                assert_eq!(path, cache);
                assert_eq!(package.digest, candidate.manifest_sha256);
                assert_eq!(listed.implementation, candidate.implementation);
                Ok(package.manifest.sequence)
            },
        )
    };
    assert_eq!(verify(&revision).unwrap(), 4);
    assert!(verify(&"0".repeat(64)).unwrap_err().is_conflict());
    assert_eq!(
        fs::read(fixture.root.join("selection.json")).unwrap(),
        selection_before
    );
    // A trial must bind the candidate and frozen host policy to the same
    // live selection and independently approved publisher and changes.
    let current_bytes = fs::read(fixture.root.join("bootstrap.json")).unwrap();
    let mut proposed = bootstrap.clone();
    proposed.policy.settings.insert(
        "amaterasu.capture".into(),
        serde_json::json!({"enabled": true}),
    );
    let proposed_bytes = serde_json::to_vec(&proposed).unwrap();
    let context = crate::policy_approval::PolicyCandidateContext {
        selection_revision: &revision,
        publisher_fingerprint: &fixture.approved.fingerprint,
        manifest_sha256: &candidate.manifest_sha256,
    };
    let approval =
        crate::policy_approval::propose_policy_approval(&current_bytes, &proposed_bytes, &context)
            .unwrap();
    let allowlist = crate::policy_approval::PolicyAllowlist {
        publisher_fingerprint: fixture.approved.fingerprint.clone(),
        changes: approval.changes.clone(),
    };
    let trial = |allowlist: &crate::policy_approval::PolicyAllowlist, bytes: &[u8]| {
        with_verified_production_trial(
            &fixture.root,
            &bootstrap,
            &store,
            &fixture.approved.id,
            catalog.digest(),
            &candidate.manifest_sha256,
            &active_candidate.manifest_sha256,
            &revision,
            100,
            ProductionTrialPolicy {
                current_bytes: &current_bytes,
                candidate_bytes: bytes,
                allowlist,
                approval: &approval,
            },
            |_path, package, trial_bootstrap| {
                assert_eq!(
                    package.runtime_config.settings.get("amaterasu.capture"),
                    trial_bootstrap.policy.settings.get("amaterasu.capture")
                );
                Ok(package.manifest.sequence)
            },
        )
    };
    assert_eq!(trial(&allowlist, &proposed_bytes).unwrap(), 4);
    let staged_digest = stage_verified_production_trial(
        &fixture.root,
        &bootstrap,
        &store,
        &fixture.approved.id,
        catalog.digest(),
        &candidate.manifest_sha256,
        &active_candidate.manifest_sha256,
        &revision,
        100,
        ProductionTrialPolicy {
            current_bytes: &current_bytes,
            candidate_bytes: &proposed_bytes,
            allowlist: &allowlist,
            approval: &approval,
        },
        |path, package, _policy| {
            assert_ne!(path, cache);
            assert_eq!(
                fs::read(path.join("capture.bin")).unwrap(),
                b"synthetic library bytes, never loaded"
            );
            Ok(package.digest.clone())
        },
    )
    .unwrap();
    assert_eq!(staged_digest, candidate.manifest_sha256);
    assert_eq!(
        fs::read(fixture.root.join("selection.json")).unwrap(),
        selection_before
    );
    let mut denied = allowlist.clone();
    denied.changes.clear();
    assert!(trial(&denied, &proposed_bytes).is_err());
    let mut changed = proposed.clone();
    changed.policy.settings.insert(
        "amaterasu.capture".into(),
        serde_json::json!({"enabled": false}),
    );
    assert!(trial(&allowlist, &serde_json::to_vec(&changed).unwrap()).is_err());
    fs::write(cache.join("capture.bin"), b"modified").unwrap();
    assert!(verify(&revision).is_err());
    fs::copy(source.join("capture.bin"), cache.join("capture.bin")).unwrap();
    store.revoke(&fixture.approved.id).unwrap();
    assert!(verify(&revision).is_err());
    assert_eq!(
        fs::read(fixture.root.join("selection.json")).unwrap(),
        selection_before
    );
}
