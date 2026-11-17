use super::*;
use crate::{
    ComponentManifest, ReleaseSignature, encode_hex,
    repository::{ApprovedRepository, Candidate, Catalog},
};
use ed25519_dalek::{Signer, SigningKey};
use std::{fs, path::PathBuf};

struct Fixture {
    root: PathBuf,
    store: RepositoryStore,
    key: SigningKey,
    source: PluginSource,
    directory: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(crate::random_identifier().unwrap());
        fs::create_dir(&root).unwrap();
        let store = RepositoryStore::new(root.join("repositories")).unwrap();
        let key = SigningKey::from_bytes(&[23; 32]);
        let public_key = key.verifying_key();
        let publisher = sha256(public_key.as_bytes());
        store
            .approve(
                ApprovedRepository::approve(
                    "fixtures".into(),
                    "https://fixtures.example".into(),
                    encode_hex(public_key.as_bytes()),
                    publisher.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        let manifest = PluginManifest {
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            id: "example.addon".into(),
            version: "1".into(),
            sequence: 7,
            expires_unix: 600,
            target: crate::current_target(),
            components: vec![ComponentManifest {
                id: "example.addon".into(),
                version: "1".into(),
                file: "addon.bin".into(),
                sha256: sha256(b"payload"),
                size: 7,
                abi_major: amaterasu_abi::ABI_MAJOR,
                runtime: false,
                requires: BTreeMap::new(),
                requested: Default::default(),
            }],
            drivers: vec![],
            api_methods: vec![],
            settings: BTreeMap::new(),
        };
        let raw = serde_json::to_vec(&manifest).unwrap();
        let manifest_digest = sha256(&raw);
        let source = PluginSource {
            repository: "fixtures".into(),
            publisher: publisher.clone(),
            catalog_digest: "a".repeat(64),
            manifest_digest,
            plugin: manifest.id,
            version: manifest.version,
            sequence: manifest.sequence,
        };
        let directory = store
            .native_candidate_cache_path(&source.repository, &source.manifest_digest)
            .unwrap();
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("manifest.json"), &raw).unwrap();
        write_json_atomically(
            &directory.join("manifest.sig"),
            &ReleaseSignature {
                key_id: publisher,
                signature: encode_hex(&key.sign(&raw).to_bytes()),
            },
        )
        .unwrap();
        fs::write(directory.join("addon.bin"), b"payload").unwrap();
        let fixture = Self {
            root,
            store,
            key,
            source,
            directory,
        };
        fixture.accept_catalog(1, 200, true);
        fixture
    }

    fn accept_catalog(&self, sequence: u64, expires_unix: u64, listed: bool) {
        let catalog = Catalog {
            schema: 1,
            repository: self.source.repository.clone(),
            sequence,
            expires_unix,
            target: crate::current_target(),
            candidates: if listed {
                vec![Candidate {
                    slot: self.source.plugin.clone(),
                    implementation: "fixture-addon".into(),
                    version: self.source.version.clone(),
                    kind: CandidateKind::NativePlugin,
                    manifest_sha256: self.source.manifest_digest.clone(),
                }]
            } else {
                vec![Candidate {
                    slot: self.source.plugin.clone(),
                    implementation: "fixture-addon".into(),
                    version: "2".into(),
                    kind: CandidateKind::NativePlugin,
                    manifest_sha256: "d".repeat(64),
                }]
            },
        };
        let raw = serde_json::to_vec(&catalog).unwrap();
        let mut signed = b"amaterasu.repository.catalog.v1\n".to_vec();
        signed.extend_from_slice(&raw);
        self.store
            .accept_catalog(
                &self.source.repository,
                &raw,
                &encode_hex(&self.key.sign(&signed).to_bytes()),
                &catalog.target,
                100,
            )
            .unwrap();
    }

    fn selection(&self) -> Selection {
        Selection {
            active: vec![self.source.clone()],
            floors: [(
                format!("{}:{}", self.source.publisher, self.source.plugin),
                (self.source.sequence, self.source.manifest_digest.clone()),
            )]
            .into(),
            ..Selection::default()
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn only_the_exact_active_overlay_can_start_after_catalog_expiry() {
    let fixture = Fixture::new();
    let mut selection = fixture.selection();
    let now = 201;
    assert!(
        fixture
            .store
            .catalog("fixtures", &crate::current_target(), now)
            .is_err()
    );
    assert_eq!(
        verify_installed_sources(&fixture.store, &selection, now).unwrap()[0]
            .manifest
            .sequence,
        7
    );
    selection.active[0].sequence += 1;
    assert_eq!(
        verify_installed_sources(&fixture.store, &selection, now)
            .err()
            .unwrap(),
        "plugin_active_floor_mismatch"
    );
    selection = fixture.selection();
    selection.floors.clear();
    assert!(verify_installed_sources(&fixture.store, &selection, now).is_err());
}

#[test]
fn expired_catalog_does_not_bypass_withdrawal_manifest_expiry_or_integrity() {
    let fixture = Fixture::new();
    let selection = fixture.selection();
    assert!(verify_installed_sources(&fixture.store, &selection, 600).is_err());
    fs::write(fixture.directory.join("addon.bin"), b"altered").unwrap();
    assert!(verify_installed_sources(&fixture.store, &selection, 201).is_err());
    fs::write(fixture.directory.join("addon.bin"), b"payload").unwrap();
    fixture.accept_catalog(2, 400, false);
    assert_eq!(
        verify_installed_sources(&fixture.store, &selection, 401)
            .err()
            .unwrap(),
        "plugin_no_longer_listed"
    );
    fixture.accept_catalog(3, 400, true);
    fixture.store.revoke("fixtures").unwrap();
    assert!(verify_installed_sources(&fixture.store, &selection, 401).is_err());
}

#[test]
fn offline_removal_is_revision_bound_and_preserves_replay_floors() {
    let fixture = Fixture::new();
    let selection = fixture.selection();
    save(&fixture.root, &selection).unwrap();
    let base = "b".repeat(64);
    assert!(remove_stopped_overlay(&fixture.root, &base, "stale").is_err());
    let expected = revision(&fixture.root, &base).unwrap();
    remove_stopped_overlay(&fixture.root, &base, &expected).unwrap();
    let result = read(&fixture.root).unwrap();
    assert!(result.active.is_empty());
    assert_eq!(result.previous, selection.active);
    assert_eq!(result.floors, selection.floors);
    let mut older = fixture.source.clone();
    older.sequence -= 1;
    assert!(check_floors(&result, &[older]).is_err());
}
