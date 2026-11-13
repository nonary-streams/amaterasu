//! Returning the host to a retained set it ran before.
//!
//! A restore changes which set is active and nothing else. The accepted
//! release never moves down: `highest_sequence` and the publisher floors stay
//! where they are, so a later update must still be newer than every release
//! the host has accepted. The set must have run healthy on this host, still be
//! on disk, verify byte for byte under the publisher the protected allowlist
//! names, and be unexpired. It is trialled and committed in the same
//! recoverable transaction as an update.
use super::*;

/// Everything the host established about a retained set before running it.
pub(super) struct RestorableSet<'a> {
    /// Where the host keeps the set's verified files.
    pub path: &'a Path,
    pub package: &'a VerifiedPackage,
    /// The policy the set is trialled and committed under.
    pub bootstrap: &'a BootstrapConfig,
    /// That policy's exact bytes, for the transaction.
    pub bootstrap_bytes: &'a [u8],
}

/// The selection after returning to `target`: the active and previous sets
/// trade places and every other record, the accepted release above all, stays.
pub(super) fn restored_selection(
    current: &ActivationState,
    target: &str,
) -> PackageResult<ActivationState> {
    if current.active.is_none()
        || current.active.as_deref() == Some(target)
        || current.pending.is_some()
        || current.trial.is_some()
    {
        return Err("package selection cannot return to this set from this state".into());
    }
    if !current.healthy_sets.contains(target) || !current.repository_sets.contains_key(target) {
        return Err("package restore requires a retained set this host ran healthy".into());
    }
    let mut next = current.clone();
    next.previous = next.active.replace(target.into());
    Ok(next)
}

/// What the accepted release permits of a retained set: at or below its
/// publisher's floor, and never a different set at the floor's own sequence.
fn require_within_accepted_release(
    selection: &ActivationState,
    publisher: &str,
    package: &VerifiedPackage,
) -> PackageResult<()> {
    let floor = selection
        .repository_floors
        .get(publisher)
        .ok_or("restored package publisher has no accepted release")?;
    if package.manifest.sequence > floor.sequence
        || package.manifest.sequence > selection.highest_sequence
        || (package.manifest.sequence == floor.sequence && package.digest != floor.digest)
    {
        return Err("restored package is not at or below the accepted release".into());
    }
    Ok(())
}

/// The signed-set half of validating a restore transition during commit and
/// crash recovery. Expiry is not rechecked here, so an interrupted restore
/// can always be rolled back; the commit path checks it before any write.
pub(super) fn validate_restored_package(
    root: &Path,
    old: &ActivationState,
    target: &str,
    proposed: &BootstrapConfig,
    origin: &amaterasu_package::RepositorySet,
) -> PackageResult<()> {
    let package = amaterasu_package::verify_package(
        &amaterasu_package::package_directory(root, target)?,
        proposed,
        amaterasu_package::VerificationPolicy {
            minimum_sequence: 0,
            allow_historical: true,
            now: 0,
        },
    )?;
    if !package.signed
        || package.digest != target
        || package.manifest.host_context_schema != 1
        || package.signer_fingerprint.as_deref() != Some(origin.key_fingerprint.as_str())
    {
        return Err("restored package is not signed by the authorized publisher".into());
    }
    require_within_accepted_release(old, &origin.key_fingerprint, &package)
}

/// Verify a retained set for a return to it and run `operation` while its
/// publisher approval stays locked. Nothing is written here. The caller holds
/// the host root lock; no browser or module supplies anything but the digest.
pub(super) fn prepare_restore<T>(
    root: &Path,
    request: &HostPackageRestore,
    operation: impl FnOnce(RestorableSet<'_>) -> PackageResult<T>,
) -> PackageResult<T> {
    let allowlist = read_allowlist(root)?;
    let original = read_bounded(&root.join("bootstrap.json"), 1024 * 1024)?;
    let bootstrap: BootstrapConfig =
        serde_json::from_slice(&original).map_err(|e| e.to_string())?;
    if bootstrap.development || bootstrap.allow_unsigned || bootstrap.trusted_keys.is_empty() {
        return Err("package restore requires a signed production bootstrap".into());
    }
    if !bootstrap
        .policy
        .principals
        .get("amaterasu.management")
        .is_some_and(|grants| {
            grants
                .call
                .contains(amaterasu_contracts::HOST_PACKAGE_ACTIVATION_SERVICE)
        })
    {
        return Err("management has not been authorized for package activation".into());
    }
    let selection_bytes = read_bounded(&root.join("selection.json"), 1024 * 1024)?;
    let selection: ActivationState =
        serde_json::from_slice(&selection_bytes).map_err(|e| e.to_string())?;
    if sha256(&selection_bytes) != request.expected_revision
        || selection.active.as_deref() != Some(request.expected_active.as_str())
    {
        return Err("package selection changed".into());
    }
    let target = request.manifest_digest.as_str();
    if !amaterasu_package::linux_user_unit::valid_digest(target) {
        return Err("invalid native package digest".into());
    }
    // Also refuses the running set, a pending trial and a set never run here.
    restored_selection(&selection, target)?;
    let origin = selection
        .repository_sets
        .get(target)
        .ok_or("restored package provenance missing")?;
    if origin.key_fingerprint != allowlist.publisher_fingerprint {
        return Err("restored package publisher differs from host authority".into());
    }
    let context = PolicyCandidateContext {
        selection_revision: &request.expected_revision,
        publisher_fingerprint: &origin.key_fingerprint,
        manifest_sha256: target,
    };
    let (proposed, _) = candidate_from_allowlist(&allowlist, &context, &original)?;
    let trial_bootstrap: BootstrapConfig =
        serde_json::from_slice(&proposed).map_err(|e| e.to_string())?;
    let path = amaterasu_package::package_directory(root, target)?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("retained package is unavailable: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("retained package is not a private package directory".into());
    }
    let store = RepositoryStore::new(root.join("repositories"))?;
    // A revocation cannot land between this check and the caller's commit.
    store.with_active_approval(&origin.repository_id, |approved| {
        if approved.fingerprint != origin.key_fingerprint
            || trial_bootstrap.trusted_keys.get(&approved.fingerprint) != Some(&approved.public_key)
        {
            return Err("restored package publisher is no longer approved and trusted".into());
        }
        // Only the pinned publisher's key may vouch for the retained bytes.
        let mut restricted = trial_bootstrap.clone();
        restricted.trusted_keys =
            [(approved.fingerprint.clone(), approved.public_key.clone())].into();
        let package = amaterasu_package::verify_package(
            &path,
            &restricted,
            amaterasu_package::VerificationPolicy {
                minimum_sequence: 0,
                allow_historical: true,
                now: unix_timestamp(),
            },
        )?;
        if !package.signed
            || package.digest != target
            || package.manifest.host_context_schema != 1
            || package.signer_fingerprint.as_deref() != Some(approved.fingerprint.as_str())
        {
            return Err("retained package signature, ABI or digest changed".into());
        }
        require_within_accepted_release(&selection, &approved.fingerprint, &package)?;
        operation(RestorableSet {
            path: &path,
            package: &package,
            bootstrap: &trial_bootstrap,
            bootstrap_bytes: &proposed,
        })
    })
}

/// Health-check a retained set, then commit policy, selection and the fixed
/// unit pin as one recoverable host transaction. The caller has stopped the
/// running components, revoked input, and holds the host root lock.
pub fn restore(
    root: &Path,
    running_pin: &str,
    state_root: &Path,
    request: &HostPackageRestore,
) -> PackageResult<()> {
    let unit = managed_unit()?;
    restore_with_unit(root, running_pin, state_root, request, &unit)
}

pub(super) fn restore_with_unit(
    root: &Path,
    running_pin: &str,
    state_root: &Path,
    request: &HostPackageRestore,
    unit: &Path,
) -> PackageResult<()> {
    let (executable, host_hash) = host_executable()?;
    commit_restore(
        root,
        running_pin,
        request,
        unit,
        &executable,
        &host_hash,
        |set| trial_set(root, state_root, set.path, set.package, set.bootstrap).map(|_| ()),
        &mut reload_user_manager,
    )
}

/// The restore transaction with its trial and unit reload supplied by the
/// caller. `trial` loads the set; it runs before anything is written.
// The explicit host identity and the two effects keep this boundary testable
// without a user manager or loadable libraries.
#[allow(clippy::too_many_arguments)]
pub(super) fn commit_restore(
    root: &Path,
    running_pin: &str,
    request: &HostPackageRestore,
    unit: &Path,
    executable: &Path,
    host_hash: &str,
    trial: impl FnOnce(&RestorableSet<'_>) -> PackageResult<()>,
    reload: &mut impl FnMut() -> PackageResult<()>,
) -> PackageResult<()> {
    if request.expected_active != running_pin {
        return Err("restore request does not match the running service".into());
    }
    let bootstrap_path = root.join("bootstrap.json");
    let selection_path = root.join("selection.json");
    let journal = root.join("package-activation-journal.json");
    let old_bootstrap = read_bounded(&bootstrap_path, 1024 * 1024)?;
    let old_selection = read_bounded(&selection_path, 1024 * 1024)?;
    let selection: ActivationState =
        serde_json::from_slice(&old_selection).map_err(|e| e.to_string())?;
    let old_unit = read_bounded(unit, 1024 * 1024)?;
    prepare_restore(root, request, |set| {
        trial(&set)?;
        let next = restored_selection(&selection, &request.manifest_digest)?;
        let new_unit = amaterasu_package::linux_user_unit::render_service_unit(
            executable,
            root,
            &request.manifest_digest,
            host_hash,
        )?;
        let transition = crate::activation_transaction::Transition {
            input_policy: prepare_input_transition(
                root,
                &request.expected_active,
                &request.manifest_digest,
            )?,
            bootstrap: crate::activation_transaction::Change {
                before: old_bootstrap,
                after: set.bootstrap_bytes.to_vec(),
            },
            selection: crate::activation_transaction::Change {
                before: old_selection,
                after: serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?,
            },
            unit: crate::activation_transaction::Change {
                before: old_unit,
                after: new_unit.into_bytes(),
            },
        };
        // The trial took time. Recheck expiry and every byte at the commit
        // boundary; recovery deliberately does not, so it can still roll back.
        let current = amaterasu_package::verify_package(
            set.path,
            set.bootstrap,
            amaterasu_package::VerificationPolicy {
                minimum_sequence: 0,
                allow_historical: true,
                now: unix_timestamp(),
            },
        )?;
        if !current.signed
            || current.digest != request.manifest_digest
            || current.signer_fingerprint != set.package.signer_fingerprint
        {
            return Err("retained package changed or expired before its restore".into());
        }
        let input_policy = root.join("input-companion-policy.json");
        let paths = crate::activation_transaction::Paths {
            input_policy: &input_policy,
            journal: &journal,
            bootstrap: &bootstrap_path,
            selection: &selection_path,
            unit,
        };
        let mut authorize = |observed: &crate::activation_transaction::Transition| {
            if observed != &transition {
                return Err("activation journal differs from the approved transition".into());
            }
            validate_transition(root, observed, executable, host_hash, running_pin)
        };
        crate::activation_transaction::commit(&paths, &transition, &mut authorize, reload)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_package::{
        ComponentManifest, NativeReleaseFloor, ReleaseManifest, ReleaseSignature, RepositorySet,
        encode_hex, random_identifier, write_json_atomically,
    };
    use ed25519_dalek::{Signer, SigningKey};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const HOST_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// A production root that ran three releases of one publisher and now
    /// runs the newest. Nothing here is loadable; trials are supplied by tests.
    struct Fixture {
        root: PathBuf,
        signer: SigningKey,
        fingerprint: String,
        /// Release 10, 11 and 12, oldest first. 12 is running.
        sets: [String; 3],
        unit: PathBuf,
    }

    impl Fixture {
        fn executable() -> &'static Path {
            Path::new("/usr/bin/amaterasu-test")
        }

        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "amaterasu-package-restore-{}",
                random_identifier().unwrap()
            ));
            fs::create_dir(&root).unwrap();
            let signer = SigningKey::from_bytes(&[73; 32]);
            let fingerprint = sha256(signer.verifying_key().as_bytes());
            let mut fixture = Self {
                unit: root.join("unit"),
                root,
                signer,
                fingerprint,
                sets: Default::default(),
            };
            fixture.sets = [10, 11, 12].map(|sequence| {
                fixture.make_set(sequence, unix_timestamp() + 3600, &fixture.signer)
            });
            let mut policy = RuntimeConfig::default();
            policy.principals.insert(
                "amaterasu.management".into(),
                Grants {
                    call: [amaterasu_contracts::HOST_PACKAGE_ACTIVATION_SERVICE.to_owned()].into(),
                    ..Default::default()
                },
            );
            let bootstrap = BootstrapConfig {
                development: false,
                allow_unsigned: false,
                trusted_keys: [(
                    fixture.fingerprint.clone(),
                    encode_hex(fixture.signer.verifying_key().as_bytes()),
                )]
                .into(),
                policy,
            };
            fs::write(
                fixture.root.join("bootstrap.json"),
                serde_json::to_vec_pretty(&bootstrap).unwrap(),
            )
            .unwrap();
            write_json_atomically(
                &fixture.root.join("package-activation-policy.json"),
                &PolicyAllowlist {
                    publisher_fingerprint: fixture.fingerprint.clone(),
                    changes: BTreeMap::new(),
                },
            )
            .unwrap();
            RepositoryStore::new(fixture.root.join("repositories"))
                .unwrap()
                .approve(
                    amaterasu_package::repository::ApprovedRepository::approve(
                        "approved".into(),
                        "https://updates.example.test".into(),
                        encode_hex(fixture.signer.verifying_key().as_bytes()),
                        fixture.fingerprint.clone(),
                    )
                    .unwrap(),
                )
                .unwrap();
            let newest = fixture.sets[2].clone();
            fixture.write_selection(&ActivationState {
                active: Some(newest.clone()),
                previous: Some(fixture.sets[1].clone()),
                highest_sequence: 12,
                repository_sets: fixture
                    .sets
                    .iter()
                    .map(|digest| (digest.clone(), fixture.provenance()))
                    .collect(),
                repository_floors: [(
                    fixture.fingerprint.clone(),
                    NativeReleaseFloor {
                        sequence: 12,
                        digest: newest.clone(),
                    },
                )]
                .into(),
                healthy_sets: fixture.sets.iter().cloned().collect(),
                ..Default::default()
            });
            fixture.write_unit(&newest);
            fixture
        }

        fn provenance(&self) -> RepositorySet {
            RepositorySet {
                repository_id: "approved".into(),
                key_fingerprint: self.fingerprint.clone(),
            }
        }

        /// Writes a signed one-file set into the host's set store.
        fn make_set(&self, sequence: u64, expires_unix: u64, signer: &SigningKey) -> String {
            let bytes = format!("runtime-{sequence}-{expires_unix}").into_bytes();
            let manifest = ReleaseManifest {
                schema: 1,
                host_context_schema: 1,
                project: "amaterasu".into(),
                sequence,
                expires_unix,
                target: amaterasu_package::current_target(),
                components: vec![ComponentManifest {
                    id: "amaterasu.runtime".into(),
                    version: "1".into(),
                    file: "runtime.bin".into(),
                    sha256: sha256(&bytes),
                    size: bytes.len() as u64,
                    abi_major: amaterasu_abi::ABI_MAJOR,
                    runtime: true,
                    requires: BTreeMap::new(),
                    requested: Grants::default(),
                }],
                privileged_requests: Vec::new(),
            };
            let raw = serde_json::to_vec(&manifest).unwrap();
            let digest = sha256(&raw);
            let directory = amaterasu_package::package_directory(&self.root, &digest).unwrap();
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("runtime.bin"), bytes).unwrap();
            fs::write(directory.join("manifest.json"), &raw).unwrap();
            write_json_atomically(
                &directory.join("manifest.sig"),
                &ReleaseSignature {
                    key_id: sha256(signer.verifying_key().as_bytes()),
                    signature: encode_hex(&signer.sign(&raw).to_bytes()),
                },
            )
            .unwrap();
            digest
        }

        fn selection(&self) -> ActivationState {
            serde_json::from_slice(&fs::read(self.root.join("selection.json")).unwrap()).unwrap()
        }

        fn write_selection(&self, selection: &ActivationState) {
            fs::write(
                self.root.join("selection.json"),
                serde_json::to_vec_pretty(selection).unwrap(),
            )
            .unwrap();
        }

        fn rendered_unit(&self, digest: &str) -> Vec<u8> {
            amaterasu_package::linux_user_unit::render_service_unit(
                Self::executable(),
                &self.root,
                digest,
                HOST_HASH,
            )
            .unwrap()
            .into_bytes()
        }

        fn write_unit(&self, digest: &str) {
            fs::write(&self.unit, self.rendered_unit(digest)).unwrap();
        }

        /// A request to return to `target` from whatever is active now.
        fn request(&self, target: &str) -> HostPackageRestore {
            HostPackageRestore {
                manifest_digest: target.into(),
                expected_active: self.selection().active.unwrap(),
                expected_revision: sha256(&fs::read(self.root.join("selection.json")).unwrap()),
            }
        }

        fn restore(&self, request: &HostPackageRestore) -> PackageResult<()> {
            commit_restore(
                &self.root,
                &request.expected_active,
                request,
                &self.unit,
                Self::executable(),
                HOST_HASH,
                |_| Ok(()),
                &mut || Ok(()),
            )
        }

        fn refusal(&self, request: &HostPackageRestore) -> String {
            let before = self.snapshot();
            let error = self.restore(request).unwrap_err();
            assert_eq!(self.snapshot(), before, "a refused restore wrote something");
            assert!(!self.journal().exists());
            error
        }

        fn journal(&self) -> PathBuf {
            self.root.join("package-activation-journal.json")
        }

        /// The three documents a transaction may change.
        fn snapshot(&self) -> [Vec<u8>; 3] {
            [
                fs::read(self.root.join("bootstrap.json")).unwrap(),
                fs::read(self.root.join("selection.json")).unwrap(),
                fs::read(&self.unit).unwrap(),
            ]
        }

        /// What `startup_transition` decides, with the unit and host identity
        /// of this fixture in place of the user manager's.
        fn startup(&self, running_pin: &str) -> StartupTransition {
            if !self.journal().exists() {
                return StartupTransition::None;
            }
            let bootstrap = self.root.join("bootstrap.json");
            let selection = self.root.join("selection.json");
            let journal = self.journal();
            let input_policy = self.root.join("input-companion-policy.json");
            let paths = crate::activation_transaction::Paths {
                input_policy: &input_policy,
                journal: &journal,
                bootstrap: &bootstrap,
                selection: &selection,
                unit: &self.unit,
            };
            evaluate_startup(
                &self.root,
                running_pin,
                &paths,
                Self::executable(),
                HOST_HASH,
                &mut || Ok(()),
            )
            .unwrap()
        }

        /// What the restored service does once it has loaded and is healthy.
        fn finalize(&self, running_pin: &str) {
            let bootstrap = self.root.join("bootstrap.json");
            let selection = self.root.join("selection.json");
            let journal = self.journal();
            let input_policy = self.root.join("input-companion-policy.json");
            let paths = crate::activation_transaction::Paths {
                input_policy: &input_policy,
                journal: &journal,
                bootstrap: &bootstrap,
                selection: &selection,
                unit: &self.unit,
            };
            crate::activation_transaction::finalize_trial(&paths, &mut |transition| {
                validate_transition(
                    &self.root,
                    transition,
                    Self::executable(),
                    HOST_HASH,
                    running_pin,
                )
            })
            .unwrap();
        }

        /// The checks a production worker makes before it loads the active set.
        fn worker_accepts_active(&self) -> PackageResult<String> {
            let active = crate::supervisor::select_active(&self.root)?;
            let bootstrap: BootstrapConfig =
                amaterasu_package::read_json(&self.root.join("bootstrap.json"))?;
            crate::publisher_selection::with_verified_selected(
                &self.root,
                &active,
                &bootstrap,
                crate::publisher_selection::SelectionPurpose::Worker,
                &mut |_, _| Ok(()),
                |package| Ok(package.digest),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_restore_moves_only_the_active_set_and_the_host_then_starts_it() {
        let fixture = Fixture::new();
        let [oldest, middle, newest] = fixture.sets.clone();
        let before = fixture.selection();
        let request = fixture.request(&oldest);
        let mut trialled = None;
        commit_restore(
            &fixture.root,
            &newest,
            &request,
            &fixture.unit,
            Fixture::executable(),
            HOST_HASH,
            |set| {
                // The trial sees the retained set itself, already verified,
                // and nothing has been written yet.
                assert_eq!(set.package.digest, oldest);
                assert_eq!(set.package.manifest.sequence, 10);
                assert_eq!(
                    set.path,
                    amaterasu_package::package_directory(&fixture.root, &oldest).unwrap()
                );
                assert_eq!(fixture.selection().active.as_deref(), Some(newest.as_str()));
                assert!(!fixture.journal().exists());
                // Its publisher cannot be revoked while the restore runs.
                assert!(
                    RepositoryStore::new(fixture.root.join("repositories"))
                        .unwrap()
                        .revoke("approved")
                        .is_err()
                );
                trialled = Some(set.package.digest.clone());
                Ok(())
            },
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(trialled.as_deref(), Some(oldest.as_str()));

        let after = fixture.selection();
        assert_eq!(after.active.as_deref(), Some(oldest.as_str()));
        assert_eq!(after.previous.as_deref(), Some(newest.as_str()));
        // The accepted release did not move.
        assert_eq!(after.highest_sequence, 12);
        assert_eq!(after.repository_floors[&fixture.fingerprint].sequence, 12);
        assert_eq!(after.repository_floors[&fixture.fingerprint].digest, newest);
        assert_eq!(after.healthy_sets, before.healthy_sets);
        assert_eq!(
            serde_json::to_value(&after.repository_sets).unwrap(),
            serde_json::to_value(&before.repository_sets).unwrap()
        );
        assert!(after.pending.is_none() && after.trial.is_none());
        assert_eq!(
            fs::read(&fixture.unit).unwrap(),
            fixture.rendered_unit(&oldest)
        );

        // The service restarts under the new pin, takes its one trial, loads
        // the older set and confirms it.
        assert_eq!(fixture.startup(&oldest), StartupTransition::Trial);
        assert_eq!(fixture.worker_accepts_active().unwrap(), oldest);
        fixture.finalize(&oldest);
        assert!(!fixture.journal().exists());
        assert_eq!(fixture.startup(&oldest), StartupTransition::None);
        assert_eq!(fixture.worker_accepts_active().unwrap(), oldest);

        // From there the host can move to any other retained set, the newest
        // accepted release included.
        fixture.restore(&fixture.request(&middle)).unwrap();
        assert_eq!(fixture.startup(&middle), StartupTransition::Trial);
        fixture.finalize(&middle);
        assert_eq!(fixture.worker_accepts_active().unwrap(), middle);
        fixture.restore(&fixture.request(&newest)).unwrap();
        assert_eq!(fixture.startup(&newest), StartupTransition::Trial);
        fixture.finalize(&newest);
        assert_eq!(fixture.worker_accepts_active().unwrap(), newest);
        let end = fixture.selection();
        assert_eq!(end.active.as_deref(), Some(newest.as_str()));
        assert_eq!(end.previous.as_deref(), Some(middle.as_str()));
        assert_eq!(end.highest_sequence, 12);
    }

    #[test]
    fn an_update_after_a_restore_must_still_exceed_the_accepted_release() {
        let fixture = Fixture::new();
        let [oldest, middle, newest] = fixture.sets.clone();
        fixture.restore(&fixture.request(&oldest)).unwrap();
        assert_eq!(fixture.startup(&oldest), StartupTransition::Trial);
        fixture.finalize(&oldest);
        let restored = fixture.selection();
        let update = |digest: &str| HostPackageActivation {
            repository_id: "approved".into(),
            catalog_digest: "1".repeat(64),
            manifest_digest: digest.into(),
            expected_active: oldest.clone(),
            expected_revision: "4".repeat(64),
        };
        // Running release 10 does not make 11 or 12 an update again.
        for (digest, sequence) in [(&middle, 11), (&newest, 12)] {
            assert!(
                next_selection(&restored, &update(digest), &fixture.fingerprint, sequence).is_err()
            );
        }
        let next = next_selection(
            &restored,
            &update(&"9".repeat(64)),
            &fixture.fingerprint,
            13,
        )
        .unwrap();
        assert_eq!(next.highest_sequence, 13);
        assert_eq!(next.previous.as_deref(), Some(oldest.as_str()));

        // The update path's own check of the running set accepts a restored one.
        let bootstrap: BootstrapConfig =
            amaterasu_package::read_json(&fixture.root.join("bootstrap.json")).unwrap();
        amaterasu_package::repository::verify_production_active(
            &fixture.root,
            &bootstrap,
            &restored,
            unix_timestamp(),
        )
        .unwrap();
        // An older active set that the host never ran healthy, or whose
        // provenance is gone, is not a restored one.
        let mut unhealthy = restored.clone();
        unhealthy.healthy_sets.remove(&oldest);
        let mut unattributed = restored.clone();
        unattributed.repository_sets.remove(&oldest);
        for selection in [unhealthy, unattributed] {
            assert!(
                amaterasu_package::repository::verify_production_active(
                    &fixture.root,
                    &bootstrap,
                    &selection,
                    unix_timestamp(),
                )
                .is_err()
            );
            fixture.write_selection(&selection);
            assert!(fixture.worker_accepts_active().is_err());
        }
    }

    #[test]
    fn a_worker_refuses_another_set_claiming_the_accepted_release_number() {
        let fixture = Fixture::new();
        let newest = fixture.sets[2].clone();
        // Signed by the same publisher, healthy and recorded, but not the
        // release this host accepted as number 12.
        let rival = fixture.make_set(12, unix_timestamp() + 7200, &fixture.signer);
        assert_ne!(rival, newest);
        let mut selection = fixture.selection();
        selection.healthy_sets.insert(rival.clone());
        selection
            .repository_sets
            .insert(rival.clone(), fixture.provenance());
        fixture.write_selection(&selection);
        assert!(
            fixture
                .refusal(&fixture.request(&rival))
                .contains("at or below the accepted release")
        );
        selection.previous = selection.active.replace(rival);
        fixture.write_selection(&selection);
        assert!(
            fixture
                .worker_accepts_active()
                .unwrap_err()
                .contains("superseded by a newer release")
        );
    }

    #[test]
    fn a_restore_whose_service_does_not_start_is_rolled_back() {
        let fixture = Fixture::new();
        let [oldest, _, newest] = fixture.sets.clone();
        let before = fixture.snapshot();
        fixture.restore(&fixture.request(&oldest)).unwrap();
        assert_ne!(fixture.snapshot(), before);
        assert_eq!(fixture.startup(&oldest), StartupTransition::Trial);
        // A second start means the one allowed trial ended before health.
        assert_eq!(fixture.startup(&oldest), StartupTransition::Recovered);
        assert_eq!(fixture.snapshot(), before);
        assert!(!fixture.journal().exists());
        assert_eq!(fixture.worker_accepts_active().unwrap(), newest);

        // A start under the old pin with a journal present also rolls back.
        fixture.restore(&fixture.request(&oldest)).unwrap();
        assert_eq!(fixture.startup(&newest), StartupTransition::Recovered);
        assert_eq!(fixture.snapshot(), before);
    }

    #[test]
    fn a_restore_interrupted_between_writes_recovers_and_a_reload_failure_undoes_it() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let [oldest, _, newest] = fixture.sets.clone();
        let before = fixture.snapshot();
        let request = fixture.request(&oldest);

        // The user manager refusing the new unit undoes every write.
        let error = commit_restore(
            &fixture.root,
            &newest,
            &request,
            &fixture.unit,
            Fixture::executable(),
            HOST_HASH,
            |_| Ok(()),
            &mut {
                let mut calls = 0;
                move || {
                    calls += 1;
                    if calls == 1 {
                        Err("reload refused".into())
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .unwrap_err();
        assert!(error.contains("reload refused"), "{error}");
        assert_eq!(fixture.snapshot(), before);
        assert!(!fixture.journal().exists());

        // A crash after the journal and the selection were written, before
        // the unit: the next start restores the old state from the journal.
        fixture.restore(&request).unwrap();
        let journal = fs::read(fixture.journal()).unwrap();
        let after = fixture.snapshot();
        fs::write(&fixture.unit, &before[2]).unwrap();
        assert_eq!(fixture.startup(&newest), StartupTransition::Recovered);
        assert_eq!(fixture.snapshot(), before);

        // A journal alone authorizes nothing: one that lowers the accepted
        // release, drops a health record or names a set the host never ran is
        // refused, and nothing is changed.
        let recorded: serde_json::Value = serde_json::from_slice(&journal).unwrap();
        let transition: crate::activation_transaction::Transition =
            serde_json::from_value(recorded["transition"].clone()).unwrap();
        let validate = |transition: &crate::activation_transaction::Transition| {
            validate_transition(
                &fixture.root,
                transition,
                Fixture::executable(),
                HOST_HASH,
                &newest,
            )
        };
        validate(&transition).unwrap();
        let forge = |edit: &dyn Fn(&mut ActivationState)| {
            let mut forged = transition.clone();
            let mut selection: ActivationState =
                serde_json::from_slice(&forged.selection.after).unwrap();
            edit(&mut selection);
            forged.selection.after = serde_json::to_vec_pretty(&selection).unwrap();
            forged
        };
        let unknown = "e".repeat(64);
        for forged in [
            forge(&|selection| selection.highest_sequence = 10),
            forge(&|selection| {
                selection
                    .repository_floors
                    .get_mut(&fixture.fingerprint)
                    .unwrap()
                    .sequence = 10;
            }),
            forge(&|selection| {
                selection.repository_floors.clear();
            }),
            forge(&|selection| {
                selection.healthy_sets.remove(&newest);
            }),
            forge(&|selection| selection.previous = None),
            forge(&|selection| selection.pending = Some(newest.clone())),
            forge(&|selection| selection.active = Some(unknown.clone())),
        ] {
            assert!(validate(&forged).is_err());
        }
        let mut unhealthy = transition.clone();
        let mut source: ActivationState =
            serde_json::from_slice(&unhealthy.selection.before).unwrap();
        source.healthy_sets.remove(&oldest);
        unhealthy.selection.before = serde_json::to_vec_pretty(&source).unwrap();
        let mut target = source.clone();
        target.previous = target.active.replace(oldest.clone());
        unhealthy.selection.after = serde_json::to_vec_pretty(&target).unwrap();
        assert!(validate(&unhealthy).is_err());
        let mut repinned = transition.clone();
        repinned.unit.after = fixture.rendered_unit(&newest);
        assert!(validate(&repinned).is_err());
        let mut repoliced = transition.clone();
        repoliced.bootstrap.after.extend_from_slice(b" ");
        assert!(validate(&repoliced).is_err());

        // A forged journal left beside committed files cannot be recovered
        // from, in either direction.
        let [bootstrap, selection, unit] = &after;
        fs::write(fixture.root.join("bootstrap.json"), bootstrap).unwrap();
        fs::write(fixture.root.join("selection.json"), selection).unwrap();
        fs::write(&fixture.unit, unit).unwrap();
        let forged = forge(&|selection| selection.highest_sequence = 10);
        fs::write(
            fixture.journal(),
            serde_json::to_vec(
                &serde_json::json!({"schema":1,"trial_started":false,"transition":forged}),
            )
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(fixture.journal(), fs::Permissions::from_mode(0o600)).unwrap();
        let bootstrap_path = fixture.root.join("bootstrap.json");
        let selection_path = fixture.root.join("selection.json");
        let journal_path = fixture.journal();
        let input_policy = fixture.root.join("input-companion-policy.json");
        let paths = crate::activation_transaction::Paths {
            input_policy: &input_policy,
            journal: &journal_path,
            bootstrap: &bootstrap_path,
            selection: &selection_path,
            unit: &fixture.unit,
        };
        assert!(
            evaluate_startup(
                &fixture.root,
                &oldest,
                &paths,
                Fixture::executable(),
                HOST_HASH,
                &mut || Ok(()),
            )
            .is_err()
        );
        assert_eq!(fixture.snapshot(), after);
    }

    #[test]
    fn a_restore_retargets_the_input_companion_with_the_same_capabilities() {
        let fixture = Fixture::new();
        let [oldest, _, newest] = fixture.sets.clone();
        let policy = fixture.root.join("input-companion-policy.json");
        let before = serde_json::to_vec(&serde_json::json!({
            "schema": 1, "component_set_digest": newest,
            "keyboard": true, "mouse": false, "gamepad": true,
        }))
        .unwrap();
        fs::write(&policy, &before).unwrap();
        fixture.restore(&fixture.request(&oldest)).unwrap();
        assert_eq!(
            fs::read(&policy).unwrap(),
            crate::input_companion::retarget_policy(&before, &newest, &oldest).unwrap()
        );
        assert_eq!(fixture.startup(&oldest), StartupTransition::Trial);
        assert_eq!(fixture.startup(&oldest), StartupTransition::Recovered);
        assert_eq!(fs::read(&policy).unwrap(), before);
    }

    #[test]
    fn a_failed_trial_writes_nothing() {
        let fixture = Fixture::new();
        let [oldest, _, newest] = fixture.sets.clone();
        let before = fixture.snapshot();
        let error = commit_restore(
            &fixture.root,
            &newest,
            &fixture.request(&oldest),
            &fixture.unit,
            Fixture::executable(),
            HOST_HASH,
            |_| Err("the set did not report health".into()),
            &mut || Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("did not report health"), "{error}");
        assert_eq!(fixture.snapshot(), before);
        assert!(!fixture.journal().exists());
    }

    #[test]
    fn a_restore_is_refused_unless_the_host_ran_trusts_and_still_holds_the_set() {
        let fixture = Fixture::new();
        let [oldest, middle, newest] = fixture.sets.clone();
        let refused = |request: &HostPackageRestore, reason: &str| {
            let error = fixture.refusal(request);
            assert!(error.contains(reason), "{error}");
        };

        // The running set, an unknown digest and a malformed one.
        refused(&fixture.request(&newest), "cannot return to this set");
        refused(&fixture.request(&"e".repeat(64)), "ran healthy");
        refused(&fixture.request("../sets"), "invalid native package digest");

        // A request made against another selection or another running set.
        let mut stale = fixture.request(&oldest);
        stale.expected_revision = "0".repeat(64);
        refused(&stale, "package selection changed");
        let mut stale = fixture.request(&oldest);
        stale.expected_active = middle.clone();
        assert!(
            commit_restore(
                &fixture.root,
                &newest,
                &stale,
                &fixture.unit,
                Fixture::executable(),
                HOST_HASH,
                |_| Ok(()),
                &mut || Ok(()),
            )
            .unwrap_err()
            .contains("does not match the running service")
        );

        // A set on disk that this host never ran healthy, or whose supplier
        // the selection no longer records.
        let original = fixture.selection();
        let mut selection = original.clone();
        selection.healthy_sets.remove(&oldest);
        fixture.write_selection(&selection);
        refused(&fixture.request(&oldest), "ran healthy");
        let mut selection = original.clone();
        selection.repository_sets.remove(&oldest);
        fixture.write_selection(&selection);
        refused(&fixture.request(&oldest), "ran healthy");

        // An activation the host has not finished.
        let mut selection = original.clone();
        selection.pending = Some(middle.clone());
        fixture.write_selection(&selection);
        refused(&fixture.request(&oldest), "cannot return to this set");

        // A set recorded under a publisher the protected allowlist does not name.
        let mut selection = original.clone();
        selection
            .repository_sets
            .get_mut(&oldest)
            .unwrap()
            .key_fingerprint = "f".repeat(64);
        fixture.write_selection(&selection);
        refused(&fixture.request(&oldest), "differs from host authority");

        // A release above the accepted one, and another set claiming its number.
        let mut selection = original.clone();
        selection.highest_sequence = 9;
        selection
            .repository_floors
            .get_mut(&fixture.fingerprint)
            .unwrap()
            .sequence = 9;
        fixture.write_selection(&selection);
        refused(
            &fixture.request(&oldest),
            "at or below the accepted release",
        );
        let mut selection = original.clone();
        selection.highest_sequence = 10;
        *selection
            .repository_floors
            .get_mut(&fixture.fingerprint)
            .unwrap() = NativeReleaseFloor {
            sequence: 10,
            digest: newest.clone(),
        };
        fixture.write_selection(&selection);
        refused(
            &fixture.request(&oldest),
            "at or below the accepted release",
        );
        let mut selection = original.clone();
        selection.repository_floors.clear();
        fixture.write_selection(&selection);
        refused(&fixture.request(&oldest), "no accepted release");
        fixture.write_selection(&original);

        // A retained set whose bytes changed, or that is gone.
        let library = amaterasu_package::package_directory(&fixture.root, &middle)
            .unwrap()
            .join("runtime.bin");
        let bytes = fs::read(&library).unwrap();
        fs::write(&library, b"changed after it was retained").unwrap();
        refused(&fixture.request(&middle), "artifact integrity failed");
        fs::write(&library, bytes).unwrap();
        let directory = amaterasu_package::package_directory(&fixture.root, &middle).unwrap();
        let moved = fixture.root.join("moved-set");
        fs::rename(&directory, &moved).unwrap();
        refused(&fixture.request(&middle), "retained package is unavailable");
        std::os::unix::fs::symlink(&moved, &directory).unwrap();
        refused(&fixture.request(&middle), "not a private package directory");
        fs::remove_file(&directory).unwrap();
        fs::rename(&moved, &directory).unwrap();

        // An expired set, and one another key signed.
        let expired = fixture.make_set(9, unix_timestamp() - 1, &fixture.signer);
        let stranger = SigningKey::from_bytes(&[74; 32]);
        let foreign = fixture.make_set(8, unix_timestamp() + 3600, &stranger);
        let mut selection = original.clone();
        for digest in [&expired, &foreign] {
            selection.healthy_sets.insert(digest.clone());
            selection
                .repository_sets
                .insert(digest.clone(), fixture.provenance());
        }
        fixture.write_selection(&selection);
        refused(&fixture.request(&expired), "expired");
        refused(&fixture.request(&foreign), "unknown release signer");
        fixture.write_selection(&original);

        // The host's own authority: management's grant, a production
        // bootstrap, the private allowlist and a live publisher approval.
        let bootstrap_path = fixture.root.join("bootstrap.json");
        let bootstrap_bytes = fs::read(&bootstrap_path).unwrap();
        let bootstrap: BootstrapConfig = serde_json::from_slice(&bootstrap_bytes).unwrap();
        let mut ungranted = bootstrap.clone();
        ungranted.policy.principals.clear();
        fs::write(
            &bootstrap_path,
            serde_json::to_vec_pretty(&ungranted).unwrap(),
        )
        .unwrap();
        refused(&fixture.request(&oldest), "has not been authorized");
        let mut development = bootstrap.clone();
        development.development = true;
        fs::write(
            &bootstrap_path,
            serde_json::to_vec_pretty(&development).unwrap(),
        )
        .unwrap();
        refused(&fixture.request(&oldest), "signed production bootstrap");
        let mut untrusting = bootstrap.clone();
        untrusting.trusted_keys = [(
            sha256(stranger.verifying_key().as_bytes()),
            encode_hex(stranger.verifying_key().as_bytes()),
        )]
        .into();
        fs::write(
            &bootstrap_path,
            serde_json::to_vec_pretty(&untrusting).unwrap(),
        )
        .unwrap();
        refused(&fixture.request(&oldest), "no longer approved and trusted");
        fs::write(&bootstrap_path, &bootstrap_bytes).unwrap();

        let allowlist_path = fixture.root.join("package-activation-policy.json");
        let allowlist = fs::read(&allowlist_path).unwrap();
        write_json_atomically(
            &allowlist_path,
            &PolicyAllowlist {
                publisher_fingerprint: "f".repeat(64),
                changes: BTreeMap::new(),
            },
        )
        .unwrap();
        refused(&fixture.request(&oldest), "differs from host authority");
        fs::remove_file(&allowlist_path).unwrap();
        fixture.refusal(&fixture.request(&oldest));
        fs::write(&allowlist_path, &allowlist).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&allowlist_path, fs::Permissions::from_mode(0o644)).unwrap();
            refused(&fixture.request(&oldest), "not a private user-owned file");
            fs::set_permissions(&allowlist_path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        // A publisher the administrator has since removed.
        assert_eq!(fixture.selection().active.as_deref(), Some(newest.as_str()));
        RepositoryStore::new(fixture.root.join("repositories"))
            .unwrap()
            .revoke("approved")
            .unwrap();
        refused(&fixture.request(&oldest), "revoked");
    }

    #[test]
    fn the_same_request_succeeds_once_nothing_stands_in_its_way() {
        let fixture = Fixture::new();
        let oldest = fixture.sets[0].clone();
        fixture.restore(&fixture.request(&oldest)).unwrap();
        assert_eq!(fixture.selection().active.as_deref(), Some(oldest.as_str()));
    }

    #[test]
    fn a_restored_selection_changes_two_fields() {
        let fixture = Fixture::new();
        let [oldest, _, newest] = fixture.sets.clone();
        let current = fixture.selection();
        let next = restored_selection(&current, &oldest).unwrap();
        let mut expected = current.clone();
        expected.active = Some(oldest.clone());
        expected.previous = Some(newest.clone());
        assert_eq!(
            serde_json::to_value(&next).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert!(restored_selection(&current, &newest).is_err());
        let mut trial = current.clone();
        trial.trial = Some(oldest.clone());
        assert!(restored_selection(&trial, &oldest).is_err());
        let mut empty = current.clone();
        empty.active = None;
        assert!(restored_selection(&empty, &oldest).is_err());
    }
}
