use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use ed25519_dalek::{Signer, SigningKey};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

struct TestPackage {
    directory: PathBuf,
    signing_key: SigningKey,
    bootstrap: BootstrapConfig,
    manifest: ReleaseManifest,
}

impl TestPackage {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("amaterasu-test-{}", random_identifier().unwrap()));
        create_private_directory(&directory).unwrap();
        let signing_key = SigningKey::from_bytes(&[42; 32]);
        let artifact = b"test artifact, never executed";
        fs::write(directory.join("runtime.bin"), artifact).unwrap();

        let bootstrap = BootstrapConfig {
            development: true,
            allow_unsigned: false,
            trusted_keys: BTreeMap::from([(
                "test-key".into(),
                encode_hex(signing_key.verifying_key().as_bytes()),
            )]),
            policy: RuntimeConfig::default(),
        };
        let manifest = ReleaseManifest {
            privileged_requests: Vec::new(),
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            sequence: 5,
            expires_unix: unix_timestamp() + 3600,
            target: current_target(),
            components: vec![ComponentManifest {
                id: "amaterasu.runtime".into(),
                version: "0.1.0".into(),
                file: "runtime.bin".into(),
                sha256: sha256(artifact),
                size: artifact.len() as u64,
                abi_major: amaterasu_abi::ABI_MAJOR,
                runtime: true,
                requires: BTreeMap::new(),
                requested: Grants::default(),
            }],
        };
        let package = Self {
            directory,
            signing_key,
            bootstrap,
            manifest,
        };
        package.write_signed_manifest();
        package
    }

    fn write_signed_manifest(&self) {
        let bytes = serde_json::to_vec_pretty(&self.manifest).unwrap();
        fs::write(self.directory.join("manifest.json"), &bytes).unwrap();
        let signature = ReleaseSignature {
            key_id: "test-key".into(),
            signature: encode_hex(&self.signing_key.sign(&bytes).to_bytes()),
        };
        write_json_atomically(&self.directory.join("manifest.sig"), &signature).unwrap();
    }

    fn verify(&self) -> PackageResult<VerifiedPackage> {
        verify_package(
            &self.directory,
            &self.bootstrap,
            VerificationPolicy {
                minimum_sequence: 5,
                allow_historical: false,
                now: unix_timestamp(),
            },
        )
    }

    fn add_feature(&mut self, id: &str, dependencies: &[&str]) {
        let mut feature = self.manifest.components[0].clone();
        feature.id = id.to_owned();
        feature.runtime = false;
        feature.file = format!("{id}.bin");
        feature.requires = dependencies
            .iter()
            .map(|dependency| ((*dependency).to_owned(), "0.1.0".to_owned()))
            .collect();
        fs::copy(
            self.directory.join("runtime.bin"),
            self.directory.join(&feature.file),
        )
        .unwrap();
        self.manifest.components.push(feature);
    }
}

impl Drop for TestPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn assert_rejected(result: PackageResult<VerifiedPackage>, expected: &str) {
    let error = result.err().expect("package must be rejected");
    assert!(error.contains(expected), "unexpected rejection: {error}");
}

#[test]
fn accepts_a_project_signed_package() {
    let package = TestPackage::new();
    assert!(package.verify().unwrap().signed);
}

#[test]
fn reports_only_completed_artifact_verification_in_manifest_order() {
    let mut package = TestPackage::new();
    package.add_feature("amaterasu.feature", &[]);
    package.write_signed_manifest();
    let policy = VerificationPolicy {
        minimum_sequence: 5,
        allow_historical: false,
        now: unix_timestamp(),
    };
    let mut progress = Vec::new();
    verify_package_with_progress(
        &package.directory,
        &package.bootstrap,
        policy,
        &mut |verified, total| {
            progress.push((verified, total));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(progress, [(0, 2), (1, 2), (2, 2)]);

    fs::write(package.directory.join("amaterasu.feature.bin"), b"tampered").unwrap();
    progress.clear();
    let failure = verify_package_with_progress(
        &package.directory,
        &package.bootstrap,
        policy,
        &mut |verified, total| {
            progress.push((verified, total));
            Ok(())
        },
    );
    assert_rejected(failure, "integrity");
    assert_eq!(progress, [(0, 2), (1, 2)]);
}

#[test]
fn progress_callback_failure_stops_before_later_artifacts() {
    let mut package = TestPackage::new();
    package.add_feature("amaterasu.feature", &[]);
    package.write_signed_manifest();
    let mut progress = Vec::new();
    let failure = verify_package_with_progress(
        &package.directory,
        &package.bootstrap,
        VerificationPolicy {
            minimum_sequence: 5,
            allow_historical: false,
            now: unix_timestamp(),
        },
        &mut |verified, total| {
            progress.push((verified, total));
            if verified == 1 {
                Err("progress rejected".into())
            } else {
                Ok(())
            }
        },
    );
    let failure = failure
        .err()
        .expect("progress failure must stop verification");
    assert_eq!(failure, "progress rejected");
    assert_eq!(progress, [(0, 2), (1, 2)]);
}

#[test]
fn rejects_modified_artifact_bytes() {
    let package = TestPackage::new();
    fs::write(package.directory.join("runtime.bin"), b"tampered").unwrap();
    assert_rejected(package.verify(), "integrity");
}

#[test]
fn verifies_the_exact_manifest_bytes() {
    let package = TestPackage::new();
    let bytes = serde_json::to_vec(&package.manifest).unwrap();
    fs::write(package.directory.join("manifest.json"), bytes).unwrap();
    assert_rejected(package.verify(), "signature");
}

#[test]
fn rejects_unknown_signers_even_when_unsigned_mode_is_enabled() {
    let mut package = TestPackage::new();
    package.bootstrap.trusted_keys.clear();
    package.bootstrap.allow_unsigned = true;
    assert_rejected(package.verify(), "unknown release signer");
}

#[test]
fn unsigned_mode_does_not_allow_official_identity_spoofing() {
    let mut package = TestPackage::new();
    fs::remove_file(package.directory.join("manifest.sig")).unwrap();
    assert_rejected(package.verify(), "unsigned");

    package.bootstrap.allow_unsigned = true;
    assert_rejected(package.verify(), "official identities");

    package.manifest.components[0].id = "custom.runtime".into();
    write_json_atomically(&package.directory.join("manifest.json"), &package.manifest).unwrap();
    assert!(!package.verify().unwrap().signed);
}

#[test]
fn rejects_expired_metadata_and_wrong_targets() {
    let mut package = TestPackage::new();
    package.manifest.expires_unix = 0;
    package.write_signed_manifest();
    assert_rejected(package.verify(), "expired");

    package.manifest.expires_unix = unix_timestamp() + 3600;
    package.manifest.target = "unsupported-target".into();
    package.write_signed_manifest();
    assert_rejected(package.verify(), "architecture");
}

#[test]
fn rejects_wrong_abi_and_missing_dependencies() {
    let mut package = TestPackage::new();
    package.manifest.components[0].abi_major = 999;
    package.write_signed_manifest();
    assert_rejected(package.verify(), "ABI");

    package.manifest.components[0].abi_major = amaterasu_abi::ABI_MAJOR;
    package.add_feature("amaterasu.feature", &["missing"]);
    package.write_signed_manifest();
    assert_rejected(package.verify(), "dependency");
}

#[test]
fn orders_feature_initialization_by_dependencies() {
    let mut package = TestPackage::new();
    package.add_feature("amaterasu.consumer", &["amaterasu.provider"]);
    package.add_feature("amaterasu.provider", &["amaterasu.runtime"]);
    package.write_signed_manifest();
    let verified = package.verify().unwrap();
    let order: Vec<_> = ordered_components(&verified.manifest.components)
        .unwrap()
        .into_iter()
        .map(|component| component.id.as_str())
        .collect();
    assert_eq!(
        order,
        [
            "amaterasu.runtime",
            "amaterasu.provider",
            "amaterasu.consumer"
        ]
    );
}

#[test]
fn rejects_dependency_cycles_before_loading_native_code() {
    let mut package = TestPackage::new();
    package.add_feature("amaterasu.first", &["amaterasu.second"]);
    package.add_feature("amaterasu.second", &["amaterasu.first"]);
    package.write_signed_manifest();
    assert_rejected(package.verify(), "cycle");
}

#[test]
fn startup_settings_are_scoped_to_selected_components() {
    let mut package = TestPackage::new();
    package.add_feature("amaterasu.feature", &[]);
    package
        .bootstrap
        .policy
        .settings
        .insert("amaterasu.feature".into(), serde_json::json!({"own": true}));
    package
        .bootstrap
        .policy
        .settings
        .insert("not.installed".into(), serde_json::json!({"secret": true}));
    package.write_signed_manifest();
    let verified = package.verify().unwrap();
    assert_eq!(verified.runtime_config.settings.len(), 1);
    assert!(
        verified
            .runtime_config
            .settings
            .contains_key("amaterasu.feature")
    );
}

#[test]
fn rejects_path_traversal() {
    let mut package = TestPackage::new();
    package.manifest.components[0].file = "../runtime.bin".into();
    package.write_signed_manifest();
    assert_rejected(package.verify(), "filename");
}

#[cfg(unix)]
#[test]
fn rejects_artifact_symlinks() {
    let package = TestPackage::new();
    fs::rename(
        package.directory.join("runtime.bin"),
        package.directory.join("other.bin"),
    )
    .unwrap();
    std::os::unix::fs::symlink("other.bin", package.directory.join("runtime.bin")).unwrap();
    assert_rejected(package.verify(), "regular bounded file");
}

#[test]
fn historical_selection_does_not_lower_the_sequence_floor() {
    let mut package = TestPackage::new();
    let host_root = package.directory.join("host");
    create_private_directory(&host_root).unwrap();
    stage_package(&package.directory, &host_root, &package.bootstrap, false).unwrap();

    package.manifest.sequence = 4;
    package.write_signed_manifest();
    assert!(stage_package(&package.directory, &host_root, &package.bootstrap, false).is_err());
    stage_package(&package.directory, &host_root, &package.bootstrap, true).unwrap();

    let selection: ActivationState = read_json(&host_root.join("selection.json")).unwrap();
    assert_eq!(selection.highest_sequence, 5);
    assert!(selection.pending.is_some());
}

#[test]
fn staging_publishes_verified_bytes_and_excludes_unknown_files() {
    let package = TestPackage::new();
    fs::write(package.directory.join("unexpected.bin"), "do not stage").unwrap();
    let host_root = package.directory.join("host");
    create_private_directory(&host_root).unwrap();
    let digest = stage_package(&package.directory, &host_root, &package.bootstrap, false).unwrap();
    let installed = package_directory(&host_root, &digest).unwrap();
    assert!(installed.join("runtime.bin").is_file());
    assert!(!installed.join("unexpected.bin").exists());
}

#[test]
fn root_lock_prevents_concurrent_mutation_and_releases_on_drop() {
    let package = TestPackage::new();
    let first = RootLock::acquire(&package.directory).unwrap();
    assert!(RootLock::acquire(&package.directory).is_err());
    drop(first);
    assert!(RootLock::acquire(&package.directory).is_ok());
}

#[path = "privileged_tests.rs"]
mod privileged_tests;
