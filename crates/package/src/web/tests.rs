use super::*;
use crate::{encode_hex, random_identifier, write_json_atomically};
use ed25519_dalek::{Signer, SigningKey};
use std::fs;

struct Fixture {
    root: PathBuf,
    signer: SigningKey,
    keys: BTreeMap<String, String>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_seed(17)
    }
    fn with_seed(seed: u8) -> Self {
        let root =
            std::env::temp_dir().join(format!("amaterasu-web-{}", random_identifier().unwrap()));
        fs::create_dir(&root).unwrap();
        let signer = SigningKey::from_bytes(&[seed; 32]);
        let keys = BTreeMap::from([(
            crate::sha256(signer.verifying_key().as_bytes()),
            encode_hex(signer.verifying_key().as_bytes()),
        )]);
        Self { root, signer, keys }
    }
    fn bundle(&self, label: &str, sequence: u64) -> PathBuf {
        let source = self.root.join(format!("source-{label}"));
        let output = self.root.join(label);
        fs::create_dir(&source).unwrap();
        fs::write(source.join("index.html"), format!("<h1>{label}</h1>")).unwrap();
        fs::write(source.join("app.js"), "document.title = 'Amaterasu';").unwrap();
        let manifest = prepare_bundle(&source, &output, sequence, 1000).unwrap();
        self.sign(&output, &manifest);
        output
    }
    fn sign(&self, directory: &Path, manifest: &Manifest) {
        let bytes = serde_json::to_vec(manifest).unwrap();
        fs::write(directory.join("manifest.json"), &bytes).unwrap();
        let signature = ReleaseSignature {
            key_id: self.keys.keys().next().unwrap().clone(),
            signature: encode_hex(&self.signer.sign(&bytes).to_bytes()),
        };
        write_json_atomically(&directory.join("manifest.sig"), &signature).unwrap();
    }
}

#[test]
fn approved_publishers_have_independent_ui_release_floors() {
    let first = Fixture::with_seed(31);
    let second = Fixture::with_seed(32);
    let root = first.root.join("publisher-installed");
    let first_key = first.keys.keys().next().unwrap();
    let second_key = second.keys.keys().next().unwrap();
    let first_v8 = first.bundle("first-v8", 8);
    let first_v9 = first.bundle("first-v9", 9);
    let second_v1 = second.bundle("second-v1", 1);
    let second_substitute = second.bundle("second-substitute", 1);

    stage_repository_bundle(&first_v8, &root, &first.keys, first_key, 10).unwrap();
    stage_repository_bundle(&first_v9, &root, &first.keys, first_key, 10).unwrap();
    stage_repository_bundle(&second_v1, &root, &second.keys, second_key, 10).unwrap();
    let selected = read_selection(&root).unwrap();
    assert_eq!(selected.highest_sequence, 9);
    assert_eq!(selected.active_sequence, Some(1));
    assert_eq!(selected.publisher_floors[first_key].sequence, 9);
    assert_eq!(selected.publisher_floors[second_key].sequence, 1);
    assert!(stage_repository_bundle(&first_v8, &root, &first.keys, first_key, 10).is_err());
    assert!(
        stage_repository_bundle(&second_substitute, &root, &second.keys, second_key, 10,).is_err()
    );
    let keys = first
        .keys
        .clone()
        .into_iter()
        .chain(second.keys.clone())
        .collect();
    assert_eq!(
        load_selected(&root, &keys, 10).unwrap().manifest.sequence,
        1
    );
}

#[test]
fn project_ui_sequence_is_independent_of_approved_publisher_sequence() {
    let project = Fixture::with_seed(41);
    let publisher = Fixture::with_seed(42);
    let root = project.root.join("mixed-installed");
    let project_v8 = project.bundle("project-v8", 8);
    let project_v9 = project.bundle("project-v9", 9);
    let publisher_v100 = publisher.bundle("publisher-v100", 100);
    let publisher_key = publisher.keys.keys().next().unwrap();
    stage_bundle(&project_v8, &root, &project.keys, 10).unwrap();
    stage_repository_bundle(&publisher_v100, &root, &publisher.keys, publisher_key, 10).unwrap();
    stage_bundle(&project_v9, &root, &project.keys, 10).unwrap();
    let selection = read_selection(&root).unwrap();
    assert_eq!(selection.highest_sequence, 100);
    assert_eq!(selection.active_sequence, Some(9));
    assert_eq!(selection.project_floor.unwrap().sequence, 9);
    assert!(stage_bundle(&project_v8, &root, &project.keys, 10).is_err());
}

#[test]
fn publisher_floor_cannot_be_reset_by_relabeling_the_same_key() {
    let publisher = Fixture::with_seed(51);
    let root = publisher.root.join("relabel-installed");
    let newer = publisher.bundle("newer", 9);
    let older = publisher.bundle("older", 8);
    let fingerprint = publisher.keys.keys().next().unwrap();
    stage_repository_bundle(&newer, &root, &publisher.keys, fingerprint, 10).unwrap();

    let false_fingerprint = "a".repeat(64);
    let relabeled = BTreeMap::from([(
        false_fingerprint.clone(),
        publisher.keys[fingerprint].clone(),
    )]);
    let mut signature: ReleaseSignature = read_json(&older.join("manifest.sig")).unwrap();
    signature.key_id = false_fingerprint.clone();
    write_json_atomically(&older.join("manifest.sig"), &signature).unwrap();
    assert!(stage_repository_bundle(&older, &root, &relabeled, &false_fingerprint, 10).is_err());
    assert_eq!(
        read_selection(&root).unwrap().publisher_floors[fingerprint].sequence,
        9
    );
}

#[test]
fn administrator_can_restore_any_retained_trusted_ui_without_erasing_floors() {
    let fixture = Fixture::new();
    let root = fixture.root.join("deep-recovery");
    let first = fixture.bundle("first", 1);
    let second = fixture.bundle("second", 2);
    let third = fixture.bundle("third", 3);
    let first_digest = stage_bundle(&first, &root, &fixture.keys, 10).unwrap();
    stage_bundle(&second, &root, &fixture.keys, 10).unwrap();
    stage_bundle(&third, &root, &fixture.keys, 10).unwrap();
    assert_eq!(
        restore_version(&root, &first_digest, &fixture.keys, 10).unwrap(),
        first_digest
    );
    let selected = read_selection(&root).unwrap();
    assert_eq!(selected.highest_sequence, 3);
    assert_eq!(selected.project_floor.unwrap().sequence, 3);
    assert_eq!(selected.active_sequence, Some(1));
    assert!(stage_bundle(&second, &root, &fixture.keys, 10).is_err());
}

#[test]
fn restaging_the_selected_ui_keeps_its_recovery_pointer() {
    let fixture = Fixture::new();
    let root = fixture.root.join("restage-recovery");
    let first = fixture.bundle("recovery-first", 1);
    let second = fixture.bundle("recovery-second", 2);
    let first_digest = stage_bundle(&first, &root, &fixture.keys, 10).unwrap();
    stage_bundle(&second, &root, &fixture.keys, 10).unwrap();
    stage_bundle(&second, &root, &fixture.keys, 10).unwrap();
    assert_eq!(
        restore_previous(&root, &fixture.keys, 10).unwrap(),
        first_digest
    );
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn signed_bundles_activate_independently_and_reject_rollback_or_equivocation() {
    let fixture = Fixture::new();
    let first = fixture.bundle("one", 1);
    let second = fixture.bundle("two", 2);
    let root = fixture.root.join("installed");
    stage_bundle(&first, &root, &fixture.keys, 10).unwrap();
    let old = load_selected(&root, &fixture.keys, 10).unwrap();
    stage_bundle(&second, &root, &fixture.keys, 10).unwrap();
    let new = load_selected(&root, &fixture.keys, 10).unwrap();
    assert_ne!(old.digest, new.digest);
    assert_eq!(old.assets["index.html"], b"<h1>one</h1>");
    assert_eq!(new.assets["index.html"], b"<h1>two</h1>");
    assert!(stage_bundle(&first, &root, &fixture.keys, 10).is_err());
    let equivocation = fixture.bundle("other-two", 2);
    assert!(stage_bundle(&equivocation, &root, &fixture.keys, 10).is_err());
    assert_eq!(
        load_selected(&root, &fixture.keys, 10).unwrap().digest,
        new.digest
    );
}

#[test]
fn administrator_can_restore_previous_ui_without_lowering_release_floor() {
    let fixture = Fixture::new();
    let first = fixture.bundle("recovery-one", 1);
    let second = fixture.bundle("recovery-two", 2);
    let root = fixture.root.join("recovery-installed");
    let first_digest = stage_bundle(&first, &root, &fixture.keys, 10).unwrap();
    stage_bundle(&second, &root, &fixture.keys, 10).unwrap();
    assert_eq!(
        restore_previous(&root, &fixture.keys, 10).unwrap(),
        first_digest
    );
    let selected = load_selected(&root, &fixture.keys, 10).unwrap();
    assert_eq!(selected.digest, first_digest);
    let selection = read_selection(&root).unwrap();
    assert_eq!(selection.highest_sequence, 2);
    assert_eq!(selection.active_sequence, Some(1));
    assert!(stage_bundle(&first, &root, &fixture.keys, 10).is_err());
}

#[test]
fn tampering_unsigned_unknown_signer_and_expiry_are_rejected() {
    let fixture = Fixture::new();
    let bundle = fixture.bundle("valid", 1);
    assert!(verify_bundle(&bundle, &BTreeMap::new(), 10).is_err());
    assert!(verify_bundle(&bundle, &fixture.keys, 1000).is_err());
    fs::write(bundle.join("app.js"), "untrusted").unwrap();
    assert!(verify_bundle(&bundle, &fixture.keys, 10).is_err());
    fs::remove_file(bundle.join("manifest.sig")).unwrap();
    assert!(verify_bundle(&bundle, &fixture.keys, 10).is_err());
}

#[test]
fn signed_metadata_cannot_escape_asset_root_or_claim_native_project() {
    let fixture = Fixture::new();
    let bundle = fixture.bundle("paths", 1);
    let mut manifest: Manifest = read_json(&bundle.join("manifest.json")).unwrap();
    manifest.project = "amaterasu".into();
    fixture.sign(&bundle, &manifest);
    assert!(verify_bundle(&bundle, &fixture.keys, 10).is_err());
    manifest.project = "amaterasu.web".into();
    let asset = manifest.files.remove("app.js").unwrap();
    for invalid in [
        "../app.js",
        "/app.js",
        "a/../app.js",
        "v1/api.js",
        "app.js%00",
        "a\\app.js",
    ] {
        manifest.files.insert(invalid.into(), asset.clone());
        fixture.sign(&bundle, &manifest);
        assert!(
            verify_bundle(&bundle, &fixture.keys, 10).is_err(),
            "{invalid}"
        );
        manifest.files.remove(invalid);
    }
}

#[cfg(unix)]
#[test]
fn nested_symlink_assets_are_rejected() {
    let fixture = Fixture::new();
    let source = fixture.root.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("index.html"), "test").unwrap();
    std::os::unix::fs::symlink(&fixture.root, source.join("linked")).unwrap();
    assert!(prepare_bundle(&source, &fixture.root.join("output"), 1, 1000).is_err());
}
