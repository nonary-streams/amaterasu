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
        let output = self.root.join("native-package");
        fs::create_dir(&output).unwrap();
        let artifact = b"synthetic library bytes, never loaded";
        fs::write(output.join("runtime.bin"), artifact).unwrap();
        fs::write(output.join("capture.bin"), artifact).unwrap();
        let component = |id: &str, file: &str, runtime| ComponentManifest {
            id: id.into(),
            version: "1.0".into(),
            file: file.into(),
            sha256: sha256(artifact),
            size: artifact.len() as u64,
            abi_major: amaterasu_abi::ABI_MAJOR,
            runtime,
            requires: BTreeMap::new(),
            requested: Grants::default(),
        };
        let manifest = ReleaseManifest {
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            sequence: 4,
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
                version: "1.0".into(),
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
