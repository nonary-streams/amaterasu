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
        let root =
            std::env::temp_dir().join(format!("amaterasu-web-{}", random_identifier().unwrap()));
        fs::create_dir(&root).unwrap();
        let signer = SigningKey::from_bytes(&[17; 32]);
        let keys = BTreeMap::from([("test".into(), encode_hex(signer.verifying_key().as_bytes()))]);
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
            key_id: "test".into(),
            signature: encode_hex(&self.signer.sign(&bytes).to_bytes()),
        };
        write_json_atomically(&directory.join("manifest.sig"), &signature).unwrap();
    }
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
