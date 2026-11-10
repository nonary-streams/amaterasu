//! Administrator changes to the approved publisher repositories.
//!
//! The plugin manager adds, refreshes and removes repositories here. Each
//! change runs on one worker thread, so the host thread never waits for the
//! network or a durable write, and at most one change is in flight.
//!
//! Approval follows the release tool's rule: the administrator supplies the
//! publisher key and a separately confirmed fingerprint. An enrolled identity
//! can never be enrolled again, so a repository is recorded only after its
//! catalog verifies under that key. Enrolling a repository lists its choices;
//! it does not load or activate any of them.
use crate::repository_transport::NetworkRepository;
use amaterasu_module_sdk::network::Network;
use amaterasu_package::repository::{
    ApprovedRepository, Catalog, CatalogCheckpoint, RepositoryStore, RepositoryTransport,
    observe_native_selection, verify_catalog,
};
use amaterasu_package::{PackageResult, current_target, unix_timestamp, web};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const LISTED_CHOICES_PER_PAGE: usize = 32;
/// Choices on one listing page. With every repository row at its longest this
/// keeps the reply under the management response limit.
const LISTED_CHOICE_BYTES: usize = 9 * 1024;
const CATALOG_BYTES: u64 = 256 * 1024;
const SIGNATURE_BYTES: u64 = 128;
/// A catalog is at most a few hundred kilobytes; a repository that cannot
/// serve one in this time is reported as unreachable.
const CATALOG_REQUEST_LIMIT: Duration = Duration::from_secs(30);
/// A console asset response holds the approval lock for a moment. A change
/// that meets it waits briefly instead of failing the administrator's request.
const STORE_ATTEMPTS: usize = 40;
const STORE_RETRY_DELAY: Duration = Duration::from_millis(25);

/// Why a repository change was refused or failed. The console words these.
type Refusal = &'static str;

/// The keys and bundle directory that decide whether the console still loads.
#[derive(Clone)]
pub(crate) struct ConsoleTrust {
    pub directory: PathBuf,
    pub trusted_keys: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationKind {
    Add,
    Refresh,
    Remove,
}

impl OperationKind {
    fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Refresh => "refresh",
            Self::Remove => "remove",
        }
    }
}

struct Job {
    operation: u64,
    repository: String,
    kind: OperationKind,
    thread: JoinHandle<Result<(), Refusal>>,
}

struct FinishedOperation {
    operation: u64,
    repository: String,
    kind: OperationKind,
    outcome: Result<(), Refusal>,
}

pub(crate) struct RepositoryAdministration {
    store_directory: PathBuf,
    console: ConsoleTrust,
    network: Network,
    cancelled: Arc<AtomicBool>,
    job: Option<Job>,
    finished: Option<FinishedOperation>,
    next_operation: u64,
    shutdown_announced: bool,
}

impl RepositoryAdministration {
    pub(crate) fn new(store_directory: PathBuf, console: ConsoleTrust, network: Network) -> Self {
        Self {
            store_directory,
            console,
            network,
            cancelled: Arc::new(AtomicBool::new(false)),
            job: None,
            finished: None,
            next_operation: 1,
            shutdown_announced: false,
        }
    }

    /// Starts the change a console request names and returns its operation
    /// number, which the repository listing reports until the next change.
    /// `package_root` is asked only for a removal.
    pub(crate) fn begin_change(
        &mut self,
        action: &str,
        payload: &Value,
        package_root: impl FnOnce() -> Option<PathBuf>,
    ) -> Value {
        let field = |name: &str| payload.get(name).and_then(Value::as_str);
        let Some(id) = field("id") else {
            return json!({"ok":false,"code":"invalid_request"});
        };
        let started = match action {
            "repository_add" => {
                let (Some(location), Some(public_key), Some(fingerprint)) =
                    (field("location"), field("public_key"), field("fingerprint"))
                else {
                    return json!({"ok":false,"code":"invalid_request"});
                };
                checked_approval(id, location, public_key, fingerprint)
                    .and_then(|approval| self.add(approval))
            }
            "repository_refresh" => self.refresh(id.to_owned()),
            "repository_remove" => self.remove(id.to_owned(), package_root()),
            _ => Err("invalid_request"),
        };
        match started {
            Ok(operation) => json!({"ok":true,"operation":operation}),
            Err(code) => json!({"ok":false,"code":code}),
        }
    }

    /// Enrolls a repository whose approval the caller has already checked.
    fn add(&mut self, approval: ApprovedRepository) -> Result<u64, Refusal> {
        let store_directory = self.store_directory.clone();
        let network = self.network.clone();
        let cancelled = self.cancelled.clone();
        self.start(approval.id.clone(), OperationKind::Add, move || {
            let store = open_store(store_directory)?;
            let transport = NetworkRepository::new(
                &approval.location,
                network,
                cancelled,
                Arc::new(AtomicU64::new(0)),
                CATALOG_REQUEST_LIMIT,
            );
            enroll(
                &store,
                approval,
                &transport,
                &current_target(),
                unix_timestamp(),
            )
        })
    }

    fn refresh(&mut self, repository: String) -> Result<u64, Refusal> {
        let store_directory = self.store_directory.clone();
        let network = self.network.clone();
        let cancelled = self.cancelled.clone();
        let id = repository.clone();
        self.start(repository, OperationKind::Refresh, move || {
            let store = open_store(store_directory)?;
            let approved = store.approved(&id).map_err(|_| "repository_unknown")?;
            let transport = NetworkRepository::new(
                &approved.location,
                network,
                cancelled,
                Arc::new(AtomicU64::new(0)),
                CATALOG_REQUEST_LIMIT,
            );
            refresh(&store, &id, &transport, &current_target(), unix_timestamp())
        })
    }

    /// Removal is permanent for the identity. `package_root` locates the
    /// native selection that must not depend on the publisher being removed.
    fn remove(
        &mut self,
        repository: String,
        package_root: Option<PathBuf>,
    ) -> Result<u64, Refusal> {
        let store_directory = self.store_directory.clone();
        let console = self.console.clone();
        let id = repository.clone();
        self.start(repository, OperationKind::Remove, move || {
            let store = open_store(store_directory)?;
            retire(
                &store,
                &id,
                package_root.as_deref(),
                &console,
                unix_timestamp(),
            )
        })
    }

    fn start(
        &mut self,
        repository: String,
        kind: OperationKind,
        work: impl FnOnce() -> Result<(), Refusal> + Send + 'static,
    ) -> Result<u64, Refusal> {
        self.advance();
        if self.shutdown_announced {
            return Err("shutting_down");
        }
        if self.job.is_some() {
            return Err("repository_busy");
        }
        let thread = thread::Builder::new()
            .name("amaterasu-repositories".into())
            .spawn(work)
            .map_err(|_| "repository_worker_unavailable")?;
        let operation = self.next_operation;
        self.next_operation += 1;
        self.finished = None;
        self.job = Some(Job {
            operation,
            repository,
            kind,
            thread,
        });
        Ok(operation)
    }

    /// Retires a finished worker without waiting for a running one.
    pub(crate) fn advance(&mut self) {
        if !self
            .job
            .as_ref()
            .is_some_and(|job| job.thread.is_finished())
        {
            return;
        }
        let job = self.job.take().expect("finished job exists");
        self.finished = Some(FinishedOperation {
            operation: job.operation,
            repository: job.repository,
            kind: job.kind,
            outcome: job
                .thread
                .join()
                .unwrap_or(Err("repository_worker_unavailable")),
        });
    }

    /// The running change, or the most recent one that finished.
    fn operation(&self) -> Value {
        if let Some(job) = &self.job {
            return json!({
                "id": job.operation,
                "repository": job.repository,
                "kind": job.kind.name(),
                "state": "running",
                "code": null,
            });
        }
        self.finished.as_ref().map_or(Value::Null, |finished| {
            json!({
                "id": finished.operation,
                "repository": finished.repository,
                "kind": finished.kind.name(),
                "state": if finished.outcome.is_ok() { "succeeded" } else { "failed" },
                "code": finished.outcome.err(),
            })
        })
    }

    /// Close admission without interrupting a change that is already running.
    pub(crate) fn prepare_shutdown(&mut self) {
        self.shutdown_announced = true;
    }

    /// Request cancellation and retire the worker once it has returned. The
    /// host keeps this module and its network provider until this is true.
    pub(crate) fn quiesce(&mut self) -> bool {
        self.shutdown_announced = true;
        self.cancelled.store(true, Ordering::Release);
        self.advance();
        self.job.is_none()
    }
}

impl Drop for RepositoryAdministration {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(job) = self.job.take() {
            let _ = job.thread.join();
        }
    }
}

/// Enrolled repositories with one page of their verified choices, and the
/// repository change that is running or last finished.
pub(crate) fn listing(
    store: &RepositoryStore,
    administration: Option<&RepositoryAdministration>,
    payload: &Value,
    now: u64,
) -> Value {
    let Ok(mut repositories) = store.list(&current_target(), now) else {
        return json!({"ok":false,"code":"repository_state_unavailable"});
    };
    let offset = payload.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut seen = 0usize;
    let mut included = 0usize;
    let mut listed_bytes = 0usize;
    for repository in &mut repositories {
        repository.candidates.retain(|candidate| {
            let bytes = serde_json::to_vec(candidate).map_or(usize::MAX, |json| json.len());
            // A page always advances, and otherwise stays inside its budget.
            let fits = included == 0 || listed_bytes.saturating_add(bytes) <= LISTED_CHOICE_BYTES;
            let keep = seen >= offset && included < LISTED_CHOICES_PER_PAGE && fits;
            seen += 1;
            if keep {
                included += 1;
                listed_bytes = listed_bytes.saturating_add(bytes);
            }
            keep
        });
    }
    let next_offset =
        (seen > offset.saturating_add(included)).then_some(offset.saturating_add(included));
    json!({
        "ok": true,
        "repositories": repositories,
        "next_offset": next_offset,
        "manageable": administration.is_some(),
        "capacity": RepositoryStore::CAPACITY,
        "operation": administration.map_or(Value::Null, RepositoryAdministration::operation),
    })
}

/// Checks a requested approval and names the field the console should flag.
fn checked_approval(
    id: &str,
    location: &str,
    public_key: &str,
    fingerprint: &str,
) -> Result<ApprovedRepository, Refusal> {
    use amaterasu_package::repository::ApprovalRejection;
    ApprovedRepository::check(
        id.into(),
        location.into(),
        public_key.into(),
        fingerprint.into(),
    )
    .map_err(|rejection| match rejection {
        ApprovalRejection::Identity => "invalid_repository_id",
        ApprovalRejection::Location => "invalid_repository_location",
        ApprovalRejection::PublicKey => "invalid_publisher_key",
        ApprovalRejection::Fingerprint => "fingerprint_mismatch",
    })
}

fn open_store(directory: PathBuf) -> Result<RepositoryStore, Refusal> {
    RepositoryStore::new(directory).map_err(|_| "repository_state_unavailable")
}

/// Records an approval only when the repository serves a catalog that this
/// publisher key signed for this host.
fn enroll(
    store: &RepositoryStore,
    approval: ApprovedRepository,
    transport: &impl RepositoryTransport,
    target: &str,
    now: u64,
) -> Result<(), Refusal> {
    let enrolled = store
        .list(target, now)
        .map_err(|_| "repository_state_unavailable")?;
    if enrolled
        .iter()
        .any(|repository| repository.id == approval.id)
    {
        return Err("repository_exists");
    }
    if enrolled.len() >= RepositoryStore::CAPACITY {
        return Err("repository_limit");
    }
    let (raw, signature) = fetch_catalog(transport)?;
    verify_catalog(
        &approval,
        &raw,
        &signature,
        &CatalogCheckpoint::default(),
        target,
        now,
    )
    .map_err(|_| catalog_refusal(&raw, &approval.id, target, now))?;
    retry_while_locked(|| store.approve(approval.clone()))
        .map_err(|_| "repository_state_unavailable")?;
    retry_while_locked(|| store.accept_catalog(&approval.id, &raw, &signature, target, now))
        .map_err(|_| "repository_state_unavailable")
}

fn refresh(
    store: &RepositoryStore,
    repository: &str,
    transport: &impl RepositoryTransport,
    target: &str,
    now: u64,
) -> Result<(), Refusal> {
    store
        .approved(repository)
        .map_err(|_| "repository_unknown")?;
    let (raw, signature) = fetch_catalog(transport)?;
    retry_while_locked(|| store.accept_catalog(repository, &raw, &signature, target, now))
        .map_err(|_| catalog_refusal(&raw, repository, target, now))
}

/// Revokes a publisher that neither the native selection nor the selected
/// console depends on. Revoking either would stop the host or lock out its
/// administrator at the next start.
fn retire(
    store: &RepositoryStore,
    repository: &str,
    package_root: Option<&Path>,
    console: &ConsoleTrust,
    now: u64,
) -> Result<(), Refusal> {
    let approved = store
        .approved(repository)
        .map_err(|_| "repository_unknown")?;
    let package_root = package_root.ok_or("repository_state_unavailable")?;
    if supplies_native_selection(package_root, repository)? {
        return Err("repository_in_use");
    }
    if console_requires(console, store, &approved.fingerprint, now)? {
        return Err("repository_serves_console");
    }
    retry_while_locked(|| store.revoke(repository)).map_err(|_| "repository_state_unavailable")
}

fn supplies_native_selection(package_root: &Path, repository: &str) -> Result<bool, Refusal> {
    let observed =
        observe_native_selection(package_root).map_err(|_| "repository_state_unavailable")?;
    let selection = &observed.selection;
    Ok([&selection.active, &selection.pending, &selection.trial]
        .into_iter()
        .flatten()
        .any(|digest| {
            observed
                .repository_sets
                .get(digest)
                .is_some_and(|set| set.repository_id == repository)
        }))
}

/// True when the selected console bundle verifies today and would stop
/// verifying without this publisher key.
fn console_requires(
    console: &ConsoleTrust,
    store: &RepositoryStore,
    fingerprint: &str,
    now: u64,
) -> Result<bool, Refusal> {
    let Ok(selection) = web::read_selection(&console.directory) else {
        return Ok(false);
    };
    let approved_keys = store
        .active_keys()
        .map_err(|_| "repository_state_unavailable")?;
    let verifies_with = |keys: &BTreeMap<String, String>| {
        web::load_version(&console.directory, &selection.digest, keys, now).is_ok()
    };
    let mut current = console.trusted_keys.clone();
    current.extend(approved_keys.clone());
    let mut remaining = console.trusted_keys.clone();
    remaining.extend(
        approved_keys
            .into_iter()
            .filter(|(key_fingerprint, _)| key_fingerprint != fingerprint),
    );
    Ok(verifies_with(&current) && !verifies_with(&remaining))
}

fn fetch_catalog(transport: &impl RepositoryTransport) -> Result<(Vec<u8>, String), Refusal> {
    let raw = transport
        .get("catalog.json", CATALOG_BYTES)
        .map_err(|_| "catalog_download_failed")?;
    let signature = transport
        .get("catalog.sig", SIGNATURE_BYTES)
        .map_err(|_| "catalog_download_failed")?;
    let signature = String::from_utf8(signature).map_err(|_| "catalog_invalid")?;
    Ok((raw, signature))
}

/// Chooses the message for a catalog that did not verify. The bytes are
/// unverified here: this only explains a refusal that was already decided.
fn catalog_refusal(raw: &[u8], repository: &str, target: &str, now: u64) -> Refusal {
    let Ok(catalog) = serde_json::from_slice::<Catalog>(raw) else {
        return "catalog_invalid";
    };
    if catalog.repository != repository {
        "catalog_identity_mismatch"
    } else if catalog.target != target {
        "catalog_wrong_platform"
    } else if catalog.expires_unix <= now {
        "catalog_expired"
    } else {
        "catalog_rejected"
    }
}

fn retry_while_locked<T>(mut change: impl FnMut() -> PackageResult<T>) -> PackageResult<T> {
    let mut attempt = 1;
    loop {
        match change() {
            Err(error)
                if attempt < STORE_ATTEMPTS && error.starts_with(amaterasu_package::ROOT_BUSY) =>
            {
                attempt += 1;
                thread::sleep(STORE_RETRY_DELAY);
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_package::repository::{Candidate, CandidateKind};
    use amaterasu_package::{
        ActivationState, RepositorySet, RootLock, encode_hex, random_identifier, sha256,
        write_json_atomically,
    };
    use ed25519_dalek::{Signer, SigningKey};
    use std::fs;
    use std::time::Instant;

    const TARGET: &str = "linux-x86_64";
    const NOW: u64 = 1_000;

    struct MemoryRepository(BTreeMap<&'static str, Vec<u8>>);

    impl RepositoryTransport for MemoryRepository {
        fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>> {
            let bytes = self.0.get(relative).ok_or("missing response")?.clone();
            if bytes.len() as u64 > maximum {
                return Err("response exceeds byte limit".into());
            }
            Ok(bytes)
        }
    }

    struct Publisher {
        signer: SigningKey,
        id: &'static str,
    }

    impl Publisher {
        fn new(id: &'static str, seed: u8) -> Self {
            Self {
                signer: SigningKey::from_bytes(&[seed; 32]),
                id,
            }
        }

        fn public_key(&self) -> String {
            encode_hex(self.signer.verifying_key().as_bytes())
        }

        fn fingerprint(&self) -> String {
            sha256(self.signer.verifying_key().as_bytes())
        }

        fn approval(&self) -> ApprovedRepository {
            checked_approval(
                self.id,
                "https://plugins.example.test/repository",
                &self.public_key(),
                &self.fingerprint(),
            )
            .unwrap()
        }

        fn catalog(&self, repository: &str, target: &str, sequence: u64, expires: u64) -> Catalog {
            Catalog {
                schema: 1,
                repository: repository.into(),
                sequence,
                expires_unix: expires,
                target: target.into(),
                candidates: vec![Candidate {
                    slot: "example.overlay".into(),
                    implementation: "example-overlay".into(),
                    version: "1.2.0".into(),
                    kind: CandidateKind::NativeSet,
                    manifest_sha256: "c".repeat(64),
                }],
            }
        }

        fn serve(&self, catalog: &Catalog) -> MemoryRepository {
            let raw = serde_json::to_vec(catalog).unwrap();
            let mut signed = b"amaterasu.repository.catalog.v1\n".to_vec();
            signed.extend_from_slice(&raw);
            let signature = encode_hex(&self.signer.sign(&signed).to_bytes());
            MemoryRepository(BTreeMap::from([
                ("catalog.json", raw),
                ("catalog.sig", signature.into_bytes()),
            ]))
        }

        fn current(&self) -> MemoryRepository {
            self.serve(&self.catalog(self.id, TARGET, 5, NOW + 100))
        }
    }

    struct Root(PathBuf);

    impl Root {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "amaterasu-repository-admin-{}",
                random_identifier().unwrap()
            ));
            fs::create_dir_all(path.join("web")).unwrap();
            Self(path)
        }

        fn store(&self) -> RepositoryStore {
            RepositoryStore::new(self.0.join("repositories")).unwrap()
        }

        fn console(&self) -> ConsoleTrust {
            ConsoleTrust {
                directory: self.0.join("web"),
                trusted_keys: BTreeMap::new(),
            }
        }

        fn select_native(&self, digest: &str, repository: Option<&str>) {
            let selection = ActivationState {
                active: Some(digest.into()),
                repository_sets: repository
                    .map(|repository| {
                        BTreeMap::from([(
                            digest.to_owned(),
                            RepositorySet {
                                repository_id: repository.into(),
                                key_fingerprint: "f".repeat(64),
                            },
                        )])
                    })
                    .unwrap_or_default(),
                ..Default::default()
            };
            write_json_atomically(&self.0.join("selection.json"), &selection).unwrap();
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn listed(store: &RepositoryStore) -> Vec<(String, bool, usize)> {
        store
            .list(TARGET, NOW)
            .unwrap()
            .into_iter()
            .map(|repository| {
                (
                    repository.id,
                    repository.revoked,
                    repository.candidates.len(),
                )
            })
            .collect()
    }

    #[test]
    fn approval_refusals_name_the_field_to_correct() {
        let publisher = Publisher::new("example-plugins", 3);
        let key = publisher.public_key();
        let fingerprint = publisher.fingerprint();
        let location = "https://plugins.example.test/repository";
        let check = |id: &str, location: &str, key: &str, fingerprint: &str| {
            checked_approval(id, location, key, fingerprint).map(|_| ())
        };
        assert_eq!(
            check("example-plugins", location, &key, &fingerprint),
            Ok(())
        );
        assert_eq!(
            check("Example Plugins", location, &key, &fingerprint),
            Err("invalid_repository_id")
        );
        assert_eq!(
            check(
                "example-plugins",
                "http://plugins.example.test",
                &key,
                &fingerprint
            ),
            Err("invalid_repository_location")
        );
        assert_eq!(
            check("example-plugins", location, "abc", &fingerprint),
            Err("invalid_publisher_key")
        );
        // The fingerprint is the administrator's independent confirmation; a
        // key that merely decodes is not enough.
        assert_eq!(
            check("example-plugins", location, &key, &"0".repeat(64)),
            Err("fingerprint_mismatch")
        );
    }

    #[test]
    fn enrollment_records_a_repository_only_after_its_catalog_verifies() {
        let root = Root::new();
        let store = root.store();
        let publisher = Publisher::new("example-plugins", 3);

        // Each rejected catalog leaves the identity free to be enrolled later.
        let impostor = Publisher::new("example-plugins", 4);
        for (served, expected) in [
            (MemoryRepository(BTreeMap::new()), "catalog_download_failed"),
            (impostor.current(), "catalog_rejected"),
            (
                publisher.serve(&publisher.catalog("another-id", TARGET, 5, NOW + 100)),
                "catalog_identity_mismatch",
            ),
            (
                publisher.serve(&publisher.catalog(publisher.id, "windows-x86_64", 5, NOW + 100)),
                "catalog_wrong_platform",
            ),
            (
                publisher.serve(&publisher.catalog(publisher.id, TARGET, 5, NOW)),
                "catalog_expired",
            ),
            (
                MemoryRepository(BTreeMap::from([
                    ("catalog.json", b"not a catalog".to_vec()),
                    ("catalog.sig", b"00".to_vec()),
                ])),
                "catalog_invalid",
            ),
        ] {
            assert_eq!(
                enroll(&store, publisher.approval(), &served, TARGET, NOW),
                Err(expected)
            );
            assert!(
                listed(&store).is_empty(),
                "{expected} enrolled a repository"
            );
        }

        enroll(
            &store,
            publisher.approval(),
            &publisher.current(),
            TARGET,
            NOW,
        )
        .unwrap();
        assert_eq!(listed(&store), [("example-plugins".into(), false, 1)]);
        assert_eq!(
            enroll(
                &store,
                publisher.approval(),
                &publisher.current(),
                TARGET,
                NOW
            ),
            Err("repository_exists")
        );
    }

    #[test]
    fn enrollment_stops_at_the_store_capacity() {
        let root = Root::new();
        let store = root.store();
        let ids: Vec<&'static str> = (0..RepositoryStore::CAPACITY)
            .map(|index| &*Box::leak(format!("publisher-{index}").into_boxed_str()))
            .collect();
        for (index, id) in ids.iter().enumerate() {
            let publisher = Publisher::new(id, index as u8 + 1);
            enroll(
                &store,
                publisher.approval(),
                &publisher.current(),
                TARGET,
                NOW,
            )
            .unwrap();
        }
        let extra = Publisher::new("one-too-many", 200);
        assert_eq!(
            enroll(&store, extra.approval(), &extra.current(), TARGET, NOW),
            Err("repository_limit")
        );
    }

    #[test]
    fn refresh_accepts_newer_catalogs_and_explains_refusals() {
        let root = Root::new();
        let store = root.store();
        let publisher = Publisher::new("example-plugins", 3);
        assert_eq!(
            refresh(&store, publisher.id, &publisher.current(), TARGET, NOW),
            Err("repository_unknown")
        );
        enroll(
            &store,
            publisher.approval(),
            &publisher.current(),
            TARGET,
            NOW,
        )
        .unwrap();

        let newer = publisher.serve(&publisher.catalog(publisher.id, TARGET, 6, NOW + 500));
        refresh(&store, publisher.id, &newer, TARGET, NOW).unwrap();
        // A replayed older catalog is refused and the newer one stays listed.
        assert_eq!(
            refresh(&store, publisher.id, &publisher.current(), TARGET, NOW),
            Err("catalog_rejected")
        );
        assert_eq!(
            refresh(
                &store,
                publisher.id,
                &MemoryRepository(BTreeMap::new()),
                TARGET,
                NOW
            ),
            Err("catalog_download_failed")
        );
        assert_eq!(
            store.list(TARGET, NOW + 200).unwrap()[0].candidates.len(),
            1,
            "the accepted catalog outlives the one it replaced"
        );
    }

    #[test]
    fn removal_refuses_a_publisher_the_running_host_depends_on() {
        let root = Root::new();
        let store = root.store();
        let publisher = Publisher::new("example-plugins", 3);
        enroll(
            &store,
            publisher.approval(),
            &publisher.current(),
            TARGET,
            NOW,
        )
        .unwrap();
        let retire_now = |package_root: Option<&Path>| {
            retire(&store, publisher.id, package_root, &root.console(), NOW)
        };

        // Without the native selection there is nothing to prove it is safe.
        assert_eq!(retire_now(None), Err("repository_state_unavailable"));
        root.select_native(&"a".repeat(64), Some(publisher.id));
        assert_eq!(retire_now(Some(&root.0)), Err("repository_in_use"));
        assert_eq!(listed(&store), [("example-plugins".into(), false, 1)]);

        // The running service holds the activation lock while this runs.
        root.select_native(&"a".repeat(64), Some("another-publisher"));
        let service_lock = RootLock::acquire(&root.0).unwrap();
        retire_now(Some(&root.0)).unwrap();
        drop(service_lock);
        assert_eq!(listed(&store), [("example-plugins".into(), true, 0)]);
        assert_eq!(retire_now(Some(&root.0)), Err("repository_unknown"));
        assert_eq!(
            enroll(
                &store,
                publisher.approval(),
                &publisher.current(),
                TARGET,
                NOW
            ),
            Err("repository_exists"),
            "a removed identity is never enrolled again"
        );
    }

    #[test]
    fn removal_refuses_the_only_publisher_that_authorizes_the_selected_console() {
        let root = Root::new();
        let store = root.store();
        let publisher = Publisher::new("example-plugins", 3);
        enroll(
            &store,
            publisher.approval(),
            &publisher.current(),
            TARGET,
            NOW,
        )
        .unwrap();
        root.select_native(&"a".repeat(64), None);

        // The selected console bundle is signed by the repository's key.
        let source = root.0.join("console-source");
        let bundle = root.0.join("console-bundle");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("index.html"), "<h1>console</h1>").unwrap();
        let manifest = web::prepare_bundle(&source, &bundle, 1, NOW + 1_000).unwrap();
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(bundle.join("manifest.json"), &manifest_bytes).unwrap();
        write_json_atomically(
            &bundle.join("manifest.sig"),
            &amaterasu_package::ReleaseSignature {
                key_id: publisher.fingerprint(),
                signature: encode_hex(&publisher.signer.sign(&manifest_bytes).to_bytes()),
            },
        )
        .unwrap();
        let publisher_keys = BTreeMap::from([(publisher.fingerprint(), publisher.public_key())]);
        web::stage_repository_bundle(
            &bundle,
            &root.0.join("web"),
            &publisher_keys,
            &publisher.fingerprint(),
            NOW,
        )
        .unwrap();

        let repository_only = root.console();
        assert_eq!(
            retire(&store, publisher.id, Some(&root.0), &repository_only, NOW),
            Err("repository_serves_console")
        );
        assert_eq!(listed(&store), [("example-plugins".into(), false, 1)]);

        // A console that no longer verifies at all is not kept alive by the
        // approval, so it does not block the removal.
        assert!(
            !console_requires(
                &repository_only,
                &store,
                &publisher.fingerprint(),
                NOW + 2_000
            )
            .unwrap()
        );

        // The host's own trusted keys also authorize this bundle: the console
        // keeps loading without the approval.
        let also_trusted = ConsoleTrust {
            directory: root.0.join("web"),
            trusted_keys: publisher_keys,
        };
        retire(&store, publisher.id, Some(&root.0), &also_trusted, NOW).unwrap();
        assert_eq!(listed(&store), [("example-plugins".into(), true, 0)]);
    }

    #[test]
    fn a_change_waits_for_a_briefly_held_approval_lock() {
        let root = Root::new();
        let store = root.store();
        let publisher = Publisher::new("example-plugins", 3);
        enroll(
            &store,
            publisher.approval(),
            &publisher.current(),
            TARGET,
            NOW,
        )
        .unwrap();
        root.select_native(&"a".repeat(64), None);

        let asset_response = RootLock::acquire(&root.0.join("repositories")).unwrap();
        let release = thread::spawn(move || {
            thread::sleep(STORE_RETRY_DELAY * 3);
            drop(asset_response);
        });
        retire(&store, publisher.id, Some(&root.0), &root.console(), NOW).unwrap();
        release.join().unwrap();
        assert_eq!(listed(&store), [("example-plugins".into(), true, 0)]);
    }

    #[test]
    fn listing_pages_choices_by_count_and_size_and_reports_the_last_change() {
        let root = Root::new();
        let store = root.store();
        let publisher = Publisher::new("example-plugins", 3);
        let mut catalog = publisher.catalog(publisher.id, &current_target(), 5, u64::MAX);
        let long = "x".repeat(120);
        catalog.candidates = (0..80)
            .map(|index| Candidate {
                slot: format!("{long}.{index:03}"),
                implementation: long.clone(),
                version: long.clone(),
                kind: CandidateKind::NativeSet,
                manifest_sha256: "c".repeat(64),
            })
            .collect();
        enroll(
            &store,
            publisher.approval(),
            &publisher.serve(&catalog),
            &current_target(),
            NOW,
        )
        .unwrap();

        let unmanaged = listing(&store, None, &json!({"action":"repository_list"}), NOW);
        assert_eq!(unmanaged["manageable"], false);
        assert_eq!(unmanaged["operation"], Value::Null);
        assert_eq!(unmanaged["capacity"], RepositoryStore::CAPACITY);
        assert_eq!(unmanaged["repositories"][0]["catalog"]["state"], "current");
        assert_eq!(unmanaged["repositories"][0]["catalog"]["sequence"], "5");

        let administration = RepositoryAdministration::new(
            root.0.join("repositories"),
            root.console(),
            Network::test_provider(),
        );
        let mut offset = 0;
        let mut slots = Vec::new();
        loop {
            let page = listing(
                &store,
                Some(&administration),
                &json!({"action":"repository_list","offset":offset}),
                NOW,
            );
            assert_eq!(page["manageable"], true);
            assert!(serde_json::to_vec(&page).unwrap().len() < 12 * 1024);
            let choices = page["repositories"][0]["candidates"].as_array().unwrap();
            assert!(!choices.is_empty() && choices.len() <= LISTED_CHOICES_PER_PAGE);
            slots.extend(choices.iter().map(|choice| choice["slot"].clone()));
            match page["next_offset"].as_u64() {
                Some(next) => {
                    assert!(next as usize > offset);
                    offset = next as usize;
                }
                None => break,
            }
        }
        assert_eq!(slots.len(), 80, "every choice is listed exactly once");
        slots.dedup();
        assert_eq!(slots.len(), 80);
    }

    #[test]
    fn requests_are_refused_before_any_work_when_they_cannot_be_approved() {
        let root = Root::new();
        let publisher = Publisher::new("example-plugins", 3);
        let mut administration = RepositoryAdministration::new(
            root.0.join("repositories"),
            root.console(),
            Network::test_provider(),
        );
        let mut asked_for_root = false;
        let mut change = |action: &str, payload: Value| {
            administration.begin_change(action, &payload, || {
                asked_for_root = true;
                None
            })
        };
        let refused = |code: &str| json!({"ok":false,"code":code});
        assert_eq!(
            change("repository_refresh", json!({})),
            refused("invalid_request")
        );
        assert_eq!(
            change("repository_add", json!({"id":"example-plugins"})),
            refused("invalid_request")
        );
        assert_eq!(
            change("repository_install", json!({"id":"example-plugins"})),
            refused("invalid_request")
        );
        assert_eq!(
            change(
                "repository_add",
                json!({
                    "id": "example-plugins",
                    "location": "https://plugins.example.test/repository",
                    "public_key": publisher.public_key(),
                    "fingerprint": "0".repeat(64),
                })
            ),
            refused("fingerprint_mismatch")
        );
        assert!(!asked_for_root, "only a removal looks up the package root");
        assert_eq!(administration.operation(), Value::Null);
        assert!(listed(&root.store()).is_empty());
    }

    #[test]
    fn one_change_runs_at_a_time_and_reports_its_result() {
        let root = Root::new();
        let publisher = Publisher::new("example-plugins", 3);
        enroll(
            &root.store(),
            publisher.approval(),
            &publisher.current(),
            TARGET,
            NOW,
        )
        .unwrap();
        let mut administration = RepositoryAdministration::new(
            root.0.join("repositories"),
            root.console(),
            Network::test_provider(),
        );
        assert_eq!(administration.operation(), Value::Null);

        let gate = Arc::new(AtomicBool::new(false));
        let waiting = gate.clone();
        let first = administration
            .start(
                "example-plugins".into(),
                OperationKind::Refresh,
                move || {
                    while !waiting.load(Ordering::Acquire) {
                        thread::yield_now();
                    }
                    Err("catalog_download_failed")
                },
            )
            .unwrap();
        assert_eq!(administration.operation()["state"], "running");
        assert_eq!(
            administration.remove("example-plugins".into(), Some(root.0.clone())),
            Err("repository_busy")
        );
        gate.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(5);
        while administration.operation()["state"] == "running" {
            administration.advance();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(
            administration.operation(),
            json!({
                "id": first,
                "repository": "example-plugins",
                "kind": "refresh",
                "state": "failed",
                "code": "catalog_download_failed",
            })
        );

        // The real removal runs on the worker against the stored approval.
        root.select_native(&"a".repeat(64), None);
        let second = administration
            .remove("example-plugins".into(), Some(root.0.clone()))
            .unwrap();
        assert!(second > first);
        while administration.operation()["state"] == "running" {
            administration.advance();
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(administration.operation()["state"], "succeeded");
        assert_eq!(administration.operation()["kind"], "remove");
        assert_eq!(listed(&root.store()), [("example-plugins".into(), true, 0)]);

        administration.prepare_shutdown();
        assert_eq!(
            administration.refresh("example-plugins".into()),
            Err("shutting_down")
        );
        assert!(administration.quiesce());
    }
}
