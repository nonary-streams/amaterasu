//! Bind a staged native digest to the publisher approved at selection time.
//! Production selections additionally require a healthy, pinned signed set.
use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, VerificationPolicy, VerifiedPackage,
    package_directory, read_json, repository::RepositoryStore, unix_timestamp,
    verify_package_with_progress,
};
use std::path::Path;

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum SelectionPurpose {
    Worker,
    ExplicitRecovery,
}

#[cfg(test)]
fn verify_selected(
    root: &Path,
    digest: &str,
    bootstrap: &BootstrapConfig,
    progress: &mut dyn FnMut(usize, usize) -> PackageResult<()>,
) -> PackageResult<VerifiedPackage> {
    with_verified_selected(
        root,
        digest,
        bootstrap,
        SelectionPurpose::Worker,
        progress,
        Ok,
    )
}

/// For a repository set, keep its approval locked until `operation` completes.
/// Native loading belongs inside this callback, before revocation can finish.
pub fn with_verified_selected<T>(
    root: &Path,
    digest: &str,
    bootstrap: &BootstrapConfig,
    purpose: SelectionPurpose,
    progress: &mut dyn FnMut(usize, usize) -> PackageResult<()>,
    operation: impl FnOnce(VerifiedPackage) -> PackageResult<T>,
) -> PackageResult<T> {
    let selection: ActivationState = read_json(&root.join("selection.json"))?;
    if let Some(source) = selection.repository_sets.get(digest) {
        if purpose == SelectionPurpose::Worker
            && selection.active.as_deref() != Some(digest)
            && selection.pending.as_deref() != Some(digest)
            && selection.trial.as_deref() != Some(digest)
        {
            return Err("repository native set lacks explicit activation authorization".into());
        }
        if !bootstrap.development
            && (purpose != SelectionPurpose::Worker
                || selection.active.as_deref() != Some(digest)
                || selection.pending.is_some()
                || selection.trial.is_some()
                || !selection.healthy_sets.contains(digest))
        {
            return Err("production repository set is not a healthy active selection".into());
        }
        let store = RepositoryStore::new(root.join("repositories"))?;
        return store.with_active_approval(&source.repository_id, |approved| {
            if approved.fingerprint != source.key_fingerprint {
                return Err("selected native publisher fingerprint changed".into());
            }
            if !bootstrap.development
                && bootstrap.trusted_keys.get(&approved.fingerprint) != Some(&approved.public_key)
            {
                return Err(
                    "selected native publisher is not trusted by the production host".into(),
                );
            }
            let floor = selection
                .repository_floors
                .get(&source.key_fingerprint)
                .ok_or("selected native publisher has no replay checkpoint")?;
            let mut restricted = bootstrap.clone();
            restricted.allow_unsigned = false;
            restricted.trusted_keys =
                [(approved.fingerprint.clone(), approved.public_key.clone())].into();
            let package = verify_package_with_progress(
                &package_directory(root, digest)?,
                &restricted,
                VerificationPolicy {
                    minimum_sequence: 0,
                    allow_historical: true,
                    now: unix_timestamp(),
                },
                progress,
            )?;
            if !package.signed
                || package.manifest.host_context_schema != 1
                || package.manifest.sequence > floor.sequence
            {
                return Err("selected repository native set lacks accepted provenance".into());
            }
            if purpose == SelectionPurpose::Worker
                && (!bootstrap.development
                    || selection.pending.as_deref() == Some(digest)
                    || selection.trial.as_deref() == Some(digest))
                && (package.manifest.sequence != floor.sequence || package.digest != floor.digest)
            {
                return Err("pending repository set was superseded by a newer release".into());
            }
            operation(package)
        });
    }
    let package = verify_package_with_progress(
        &package_directory(root, digest)?,
        bootstrap,
        VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: true,
            now: unix_timestamp(),
        },
        progress,
    )?;
    operation(package)
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_package::repository::ApprovedRepository;
    use amaterasu_package::{
        ComponentManifest, NativeReleaseFloor, ReleaseManifest, ReleaseSignature, RepositorySet,
        current_target, encode_hex, random_identifier, sha256, write_json_atomically,
    };
    use ed25519_dalek::{Signer, SigningKey};
    use std::collections::BTreeMap;
    use std::fs;

    #[test]
    fn worker_verification_binds_selected_digest_to_live_publisher_approval() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-publisher-worker-test-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let signer = SigningKey::from_bytes(&[61; 32]);
        let key = encode_hex(signer.verifying_key().as_bytes());
        let fingerprint = sha256(signer.verifying_key().as_bytes());
        let approved = ApprovedRepository::approve(
            "synthetic".into(),
            "https://updates.example.test".into(),
            key,
            fingerprint.clone(),
        )
        .unwrap();
        let store = RepositoryStore::new(root.join("repositories")).unwrap();
        store.approve(approved).unwrap();
        let bootstrap = BootstrapConfig {
            development: true,
            allow_unsigned: false,
            trusted_keys: BTreeMap::new(),
            policy: RuntimeConfig::default(),
        };
        write_json_atomically(&root.join("bootstrap.json"), &bootstrap).unwrap();
        let artifact = b"synthetic runtime bytes, never loaded";
        let manifest = ReleaseManifest {
            privileged_requests: Vec::new(),
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            sequence: 4,
            expires_unix: unix_timestamp() + 3600,
            target: current_target(),
            components: vec![ComponentManifest {
                id: "amaterasu.runtime".into(),
                version: "1".into(),
                file: "runtime.bin".into(),
                sha256: sha256(artifact),
                size: artifact.len() as u64,
                abi_major: amaterasu_abi::ABI_MAJOR,
                runtime: true,
                requires: BTreeMap::new(),
                requested: Grants::default(),
            }],
        };
        let raw = serde_json::to_vec(&manifest).unwrap();
        let digest = sha256(&raw);
        let directory = package_directory(&root, &digest).unwrap();
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("runtime.bin"), artifact).unwrap();
        fs::write(directory.join("manifest.json"), &raw).unwrap();
        write_json_atomically(
            &directory.join("manifest.sig"),
            &ReleaseSignature {
                key_id: fingerprint.clone(),
                signature: encode_hex(&signer.sign(&raw).to_bytes()),
            },
        )
        .unwrap();
        let mut selection = ActivationState::default();
        selection.repository_sets.insert(
            digest.clone(),
            RepositorySet {
                repository_id: "synthetic".into(),
                key_fingerprint: fingerprint.clone(),
            },
        );
        selection.repository_floors.insert(
            fingerprint,
            NativeReleaseFloor {
                sequence: 4,
                digest: digest.clone(),
            },
        );
        write_json_atomically(&root.join("selection.json"), &selection).unwrap();
        assert!(verify_selected(&root, &digest, &bootstrap, &mut |_, _| Ok(())).is_err());
        selection.pending = Some(digest.clone());
        write_json_atomically(&root.join("selection.json"), &selection).unwrap();
        assert!(crate::native_recovery::restore(&root, Some(&digest)).is_err());
        assert!(verify_selected(&root, &digest, &bootstrap, &mut |_, _| Ok(())).is_ok());
        with_verified_selected(
            &root,
            &digest,
            &bootstrap,
            SelectionPurpose::Worker,
            &mut |_, _| Ok(()),
            |_| {
                assert!(store.revoke("synthetic").is_err());
                Ok(())
            },
        )
        .unwrap();
        let mut production = bootstrap.clone();
        production.development = false;
        assert!(verify_selected(&root, &digest, &production, &mut |_, _| Ok(())).is_err());
        let approved = store.approved("synthetic").unwrap();
        production
            .trusted_keys
            .insert(approved.fingerprint.clone(), approved.public_key.clone());
        selection.pending = None;
        selection.active = Some(digest.clone());
        write_json_atomically(&root.join("selection.json"), &selection).unwrap();
        assert!(verify_selected(&root, &digest, &production, &mut |_, _| Ok(())).is_err());
        selection.healthy_sets.insert(digest.clone());
        write_json_atomically(&root.join("selection.json"), &selection).unwrap();
        assert!(verify_selected(&root, &digest, &production, &mut |_, _| Ok(())).is_ok());
        selection.active = None;
        selection.pending = Some(digest.clone());
        write_json_atomically(&root.join("selection.json"), &selection).unwrap();
        selection
            .repository_floors
            .values_mut()
            .next()
            .unwrap()
            .sequence = 5;
        selection
            .repository_floors
            .values_mut()
            .next()
            .unwrap()
            .digest = "f".repeat(64);
        write_json_atomically(&root.join("selection.json"), &selection).unwrap();
        assert!(verify_selected(&root, &digest, &bootstrap, &mut |_, _| Ok(())).is_err());
        assert!(
            with_verified_selected(
                &root,
                &digest,
                &bootstrap,
                SelectionPurpose::ExplicitRecovery,
                &mut |_, _| Ok(()),
                |_| Ok(())
            )
            .is_ok()
        );
        selection
            .repository_floors
            .values_mut()
            .next()
            .unwrap()
            .sequence = 3;
        write_json_atomically(&root.join("selection.json"), &selection).unwrap();
        assert!(verify_selected(&root, &digest, &bootstrap, &mut |_, _| Ok(())).is_err());
        selection
            .repository_floors
            .values_mut()
            .next()
            .unwrap()
            .sequence = 4;
        selection
            .repository_floors
            .values_mut()
            .next()
            .unwrap()
            .digest = digest.clone();
        selection.pending = None;
        selection.active = Some("e".repeat(64));
        selection.healthy_sets.insert(digest.clone());
        write_json_atomically(&root.join("selection.json"), &selection).unwrap();
        crate::native_recovery::restore(&root, Some(&digest)).unwrap();
        let restored: ActivationState = read_json(&root.join("selection.json")).unwrap();
        assert_eq!(restored.active.as_deref(), Some(digest.as_str()));
        assert_eq!(restored.previous.as_deref(), selection.active.as_deref());
        store.revoke("synthetic").unwrap();
        assert!(verify_selected(&root, &digest, &bootstrap, &mut |_, _| Ok(())).is_err());
    }
}
