use super::*;
use std::cell::{Cell, RefCell};

const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef01234567";

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    destination: PathBuf,
    runtime: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "amaterasu-vhf-installer-{}",
                amaterasu_storage::random_identifier().unwrap()
            ));
        let source = root.join("source");
        let destination = root.join("installed");
        let runtime = root.join("runtime");
        fs::create_dir_all(source.join("driver")).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        let mut files = BTreeMap::new();
        for name in FILES {
            let path = source.join(name);
            fs::write(&path, format!("fixture for {name}")).unwrap();
            files.insert(name.into(), file_hash(&path).unwrap());
        }
        write_json_atomically(
            &source.join("manifest.json"),
            &Manifest {
                schema: 1,
                architecture: "x64".into(),
                hardware_id: HARDWARE_ID.into(),
                service_name: SERVICE_NAME.into(),
                files,
            },
        )
        .unwrap();
        Self {
            root,
            source,
            destination,
            runtime,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct FixtureSignatures {
    signer: String,
    reject_member: Cell<bool>,
}

impl Default for FixtureSignatures {
    fn default() -> Self {
        Self {
            signer: FINGERPRINT.into(),
            reject_member: Cell::new(false),
        }
    }
}

impl Signatures for FixtureSignatures {
    fn verify_catalog_member(&self, _: &Path, _: &Path) -> PackageResult<()> {
        if self.reject_member.get() {
            Err("catalog does not include payload".into())
        } else {
            Ok(())
        }
    }

    fn verified_signer(&self, _: &Path) -> PackageResult<String> {
        Ok(self.signer.clone())
    }
}

#[derive(Default)]
struct FixtureActions {
    service: Cell<bool>,
    device: RefCell<Option<(String, String)>>,
    published_inf: RefCell<Option<String>>,
    fail_service: Cell<bool>,
    fail_service_after_create: Cell<bool>,
    fail_driver_after_create: Cell<bool>,
    service_running: Cell<bool>,
    events: RefCell<Vec<String>>,
}

impl InstallActions for FixtureActions {
    fn ensure_ready(&self, destination: &Path) -> PackageResult<()> {
        fs::create_dir_all(destination).map_err(|error| error.to_string())
    }

    fn service_absent(&self) -> PackageResult<bool> {
        Ok(!self.service.get())
    }

    fn service_stopped(&self) -> PackageResult<()> {
        if self.service_running.get() {
            Err("companion still running".into())
        } else {
            Ok(())
        }
    }

    fn create_service(&self, _: &Path, _: &Path) -> PackageResult<()> {
        if self.fail_service.get() {
            return Err("synthetic SCM failure".into());
        }
        self.service.set(true);
        self.events.borrow_mut().push("service created".into());
        if self.fail_service_after_create.get() {
            return Err("synthetic service SID setup failure".into());
        }
        Ok(())
    }

    fn service_matches(&self, _: &Path, _: &Path) -> PackageResult<bool> {
        Ok(self.service.get())
    }

    fn remove_service(&self) -> PackageResult<()> {
        self.service.set(false);
        self.events.borrow_mut().push("service removed".into());
        Ok(())
    }

    fn install_device(&self, _: &Path) -> PackageResult<(String, String)> {
        let identity = ("ROOT\\SYSTEM\\0000".into(), "oem123.inf".into());
        *self.device.borrow_mut() = Some(identity.clone());
        *self.published_inf.borrow_mut() = Some("oem123.inf".into());
        if self.fail_driver_after_create.get() {
            return Err("simulated lost driver response".into());
        }
        self.events.borrow_mut().push("device created".into());
        Ok(identity)
    }

    fn find_owned_device(&self) -> PackageResult<Option<(String, String)>> {
        Ok(self.device.borrow().clone())
    }

    fn find_matching_published_inf(&self, _: &Path) -> PackageResult<Option<String>> {
        Ok(self.published_inf.borrow().clone())
    }

    fn published_inf_matches(&self, inf: &str, _: &Path) -> PackageResult<bool> {
        Ok(inf == "oem123.inf")
    }

    fn remove_device(&self, instance: &str) -> PackageResult<()> {
        assert_eq!(instance, "ROOT\\SYSTEM\\0000");
        *self.device.borrow_mut() = None;
        self.events.borrow_mut().push("device removed".into());
        Ok(())
    }

    fn remove_published_inf(&self, inf: &str) -> PackageResult<()> {
        assert_eq!(inf, "oem123.inf");
        *self.published_inf.borrow_mut() = None;
        self.events.borrow_mut().push("oem123 removed".into());
        Ok(())
    }
}

#[test]
fn payload_mutation_and_wrong_signer_fail_before_any_install_action() {
    let fixture = Fixture::new();
    let mut signatures = FixtureSignatures::default();
    let actions = FixtureActions::default();
    fs::write(fixture.source.join(FILES[1]), "changed").unwrap();
    assert!(
        install(
            &fixture.source,
            &fixture.destination,
            &fixture.runtime,
            FINGERPRINT,
            &signatures,
            &actions
        )
        .is_err()
    );
    assert!(!fixture.destination.exists());
    fs::write(
        fixture.source.join(FILES[1]),
        format!("fixture for {}", FILES[1]),
    )
    .unwrap();
    signatures.signer = "f".repeat(40);
    assert!(
        install(
            &fixture.source,
            &fixture.destination,
            &fixture.runtime,
            FINGERPRINT,
            &signatures,
            &actions
        )
        .is_err()
    );
    assert!(!fixture.destination.exists());
    signatures.signer = FINGERPRINT.into();
    signatures.reject_member.set(true);
    assert!(
        install(
            &fixture.source,
            &fixture.destination,
            &fixture.runtime,
            FINGERPRINT,
            &signatures,
            &actions
        )
        .is_err()
    );
    assert!(actions.events.borrow().is_empty());
    signatures.reject_member.set(false);
    fs::write(fixture.source.join(FILES[2]), "tampered catalog").unwrap();
    assert!(verify_package(&fixture.source, FINGERPRINT, &signatures).is_err());
}

#[test]
fn sealing_requires_an_authenticated_catalog_and_companion() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.source.join("manifest.json")).unwrap();
    let mut signatures = FixtureSignatures::default();
    signatures.reject_member.set(true);
    assert!(seal_package(&fixture.source, FINGERPRINT, &signatures).is_err());
    assert!(!fixture.source.join("manifest.json").exists());
    signatures.reject_member.set(false);
    signatures.signer = "f".repeat(40);
    assert!(seal_package(&fixture.source, FINGERPRINT, &signatures).is_err());
    signatures.signer = FINGERPRINT.into();
    seal_package(&fixture.source, FINGERPRINT, &signatures).unwrap();
    assert!(verify_package(&fixture.source, FINGERPRINT, &signatures).is_ok());
}

#[test]
fn service_failure_leaves_exact_journal_and_rolls_back_only_owned_files() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    actions.fail_service.set(true);
    assert!(
        install(
            &fixture.source,
            &fixture.destination,
            &fixture.runtime,
            FINGERPRINT,
            &FixtureSignatures::default(),
            &actions
        )
        .is_err()
    );
    let journal: InstallJournal = read_json(&journal_path(&fixture.destination)).unwrap();
    assert!(journal.service_pending);
    assert!(journal.device_instance.is_none());
    rollback(&fixture.destination, &fixture.runtime, &actions).unwrap();
    assert!(!journal_path(&fixture.destination).exists());
    assert!(!fixture.destination.join("driver").exists());
    assert!(fixture.source.join(FILES[0]).exists());
    assert!(actions.events.borrow().is_empty());
}

#[test]
fn partial_service_creation_is_reconciled_by_exact_service_identity() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    actions.fail_service_after_create.set(true);
    assert!(
        install(
            &fixture.source,
            &fixture.destination,
            &fixture.runtime,
            FINGERPRINT,
            &FixtureSignatures::default(),
            &actions
        )
        .is_err()
    );
    assert!(actions.service.get());
    rollback(&fixture.destination, &fixture.runtime, &actions).unwrap();
    assert!(!actions.service.get());
    assert_eq!(
        *actions.events.borrow(),
        ["service created", "service removed"]
    );
}

#[test]
fn interrupted_driver_operation_reconciles_only_the_exact_owned_identity() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    actions.fail_driver_after_create.set(true);
    assert!(
        install(
            &fixture.source,
            &fixture.destination,
            &fixture.runtime,
            FINGERPRINT,
            &FixtureSignatures::default(),
            &actions
        )
        .is_err()
    );
    rollback(&fixture.destination, &fixture.runtime, &actions).unwrap();
    assert!(!actions.service.get());
    assert!(actions.device.borrow().is_none());
    assert!(!journal_path(&fixture.destination).exists());
    assert!(!fixture.destination.join("driver").exists());
}

#[test]
fn successful_rollback_targets_only_recorded_device_and_service() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    install(
        &fixture.source,
        &fixture.destination,
        &fixture.runtime,
        FINGERPRINT,
        &FixtureSignatures::default(),
        &actions,
    )
    .unwrap();
    rollback(&fixture.destination, &fixture.runtime, &actions).unwrap();
    assert_eq!(
        *actions.events.borrow(),
        [
            "service created",
            "device created",
            "device removed",
            "oem123 removed",
            "service removed"
        ]
    );
    assert!(fixture.source.join(FILES[0]).exists());
    assert!(!journal_path(&fixture.destination).exists());
}

#[test]
fn changed_identity_or_file_cannot_be_deleted() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    install(
        &fixture.source,
        &fixture.destination,
        &fixture.runtime,
        FINGERPRINT,
        &FixtureSignatures::default(),
        &actions,
    )
    .unwrap();
    *actions.device.borrow_mut() = Some(("ROOT\\SOMETHINGELSE\\0000".into(), "oem123.inf".into()));
    assert!(rollback(&fixture.destination, &fixture.runtime, &actions).is_err());
    assert!(actions.service.get());
    *actions.device.borrow_mut() = Some(("ROOT\\SYSTEM\\0000".into(), "oem123.inf".into()));
    fs::write(fixture.destination.join(FILES[1]), "changed").unwrap();
    assert!(rollback(&fixture.destination, &fixture.runtime, &actions).is_err());
    assert!(fixture.destination.join(FILES[1]).exists());
}

#[test]
fn rollback_requires_explicit_grant_revocation() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    install(
        &fixture.source,
        &fixture.destination,
        &fixture.runtime,
        FINGERPRINT,
        &FixtureSignatures::default(),
        &actions,
    )
    .unwrap();
    fs::write(
        fixture.runtime.join("input-companion-policy.json"),
        "synthetic grant",
    )
    .unwrap();
    assert!(rollback(&fixture.destination, &fixture.runtime, &actions).is_err());
    assert!(actions.device.borrow().is_some());
}

#[test]
fn existing_companion_is_never_claimed_by_new_install() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    actions.service.set(true);
    assert!(
        install(
            &fixture.source,
            &fixture.destination,
            &fixture.runtime,
            FINGERPRINT,
            &FixtureSignatures::default(),
            &actions,
        )
        .is_err()
    );
    assert!(!journal_path(&fixture.destination).exists());
    assert!(actions.service.get());
    assert!(actions.events.borrow().is_empty());
}

#[test]
fn running_companion_blocks_rollback_before_device_mutation() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    install(
        &fixture.source,
        &fixture.destination,
        &fixture.runtime,
        FINGERPRINT,
        &FixtureSignatures::default(),
        &actions,
    )
    .unwrap();
    actions.service_running.set(true);
    assert!(rollback(&fixture.destination, &fixture.runtime, &actions).is_err());
    assert!(actions.device.borrow().is_some());
    assert_eq!(
        *actions.events.borrow(),
        ["service created", "device created"]
    );
}

#[test]
fn interrupted_copy_intent_recovers_partial_owned_file() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    fs::create_dir_all(fixture.destination.join("driver")).unwrap();
    fs::write(fixture.destination.join(FILES[0]), "partial copy").unwrap();
    let mut pending = BTreeMap::new();
    pending.insert(FILES[0].into(), "0".repeat(64));
    write_json_atomically(
        &journal_path(&fixture.destination),
        &InstallJournal {
            schema: 1,
            package_digest: "0".repeat(64),
            staged_files: BTreeMap::new(),
            staging_pending: pending,
            service_pending: false,
            service_created: false,
            driver_pending: false,
            device_instance: None,
            published_inf: None,
        },
    )
    .unwrap();
    rollback(&fixture.destination, &fixture.runtime, &actions).unwrap();
    assert!(!fixture.destination.join(FILES[0]).exists());
    assert!(!fixture.destination.join("driver").exists());
    assert!(!journal_path(&fixture.destination).exists());
}

#[test]
fn driver_recovery_handles_unbound_root_device_and_published_inf() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    actions.fail_driver_after_create.set(true);
    assert!(
        install(
            &fixture.source,
            &fixture.destination,
            &fixture.runtime,
            FINGERPRINT,
            &FixtureSignatures::default(),
            &actions,
        )
        .is_err()
    );
    *actions.device.borrow_mut() = Some(("ROOT\\SYSTEM\\0000".into(), String::new()));
    rollback(&fixture.destination, &fixture.runtime, &actions).unwrap();
    assert!(actions.device.borrow().is_none());
    assert!(actions.published_inf.borrow().is_none());
}

#[test]
fn preexisting_published_inf_is_not_claimed() {
    let fixture = Fixture::new();
    let actions = FixtureActions::default();
    *actions.published_inf.borrow_mut() = Some("oem123.inf".into());
    assert!(
        install(
            &fixture.source,
            &fixture.destination,
            &fixture.runtime,
            FINGERPRINT,
            &FixtureSignatures::default(),
            &actions,
        )
        .is_err()
    );
    assert!(!journal_path(&fixture.destination).exists());
    assert_eq!(
        actions.published_inf.borrow().as_deref(),
        Some("oem123.inf")
    );
}
