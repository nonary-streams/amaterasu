//! A publisher with a throwaway key, the repository it serves from a
//! directory, and a host root that runs one of its sets. Nothing here opens a
//! socket or loads a library.
use super::Action;
use super::environment::Environment;
use super::service::{ActivationHost, Admission, HostActivationState, Service};
use super::transport::Connector;
use amaterasu_contracts::{Grants, HostPackageActivation, HostPackageRestore, RuntimeConfig};
use amaterasu_package::repository::{
    ApprovedRepository, Candidate, CandidateKind, Catalog, RepositoryStore, RepositoryTransport,
};
use amaterasu_package::{
    ActivationState, BootstrapConfig, ComponentManifest, PackageResult, ReleaseManifest,
    ReleaseSignature, RepositorySet, current_target, encode_hex, package_directory,
    random_identifier, sha256, unix_timestamp, write_json_atomically,
};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

mod automatic;
mod end_to_end;
mod phases;
mod switching_and_automatic;
mod versions;

const REPOSITORY: &str = "amaterasu";
const SLOT: &str = "amaterasu.runtime";
const IMPLEMENTATION: &str = "amaterasu-linux-session";
const CATALOG_DOMAIN: &[u8] = b"amaterasu.repository.catalog.v1\n";

/// One module of a set: its identity, version and library bytes.
#[derive(Clone, Copy)]
struct Module {
    id: &'static str,
    version: &'static str,
    contents: &'static [u8],
}

fn module(id: &'static str, version: &'static str, contents: &'static [u8]) -> Module {
    Module {
        id,
        version,
        contents,
    }
}

/// The set most tests start from.
fn first_release() -> Vec<Module> {
    vec![
        module(SLOT, "0.1.0", b"runtime library, first build"),
        module("amaterasu.display", "0.1.0", b"display library"),
        module("amaterasu.input", "0.1.0", b"input library, first build"),
    ]
}

/// A release that changes two modules, keeps one and adds one.
fn second_release() -> Vec<Module> {
    vec![
        module(SLOT, "0.2.0", b"runtime library, second build"),
        module("amaterasu.display", "0.1.0", b"display library"),
        module("amaterasu.input", "0.2.0", b"input library, second build"),
        module("amaterasu.stats", "0.1.0", b"statistics library"),
    ]
}

fn library_name(id: &str) -> String {
    format!("{id}.so")
}

/// A signed native set: every file by name, as a repository serves it.
struct SignedSet {
    digest: String,
    version: String,
    files: BTreeMap<String, Vec<u8>>,
}

impl SignedSet {
    fn write_to(&self, directory: &Path) {
        fs::create_dir_all(directory).unwrap();
        for (name, bytes) in &self.files {
            fs::write(directory.join(name), bytes).unwrap();
        }
    }

    /// How a catalog lists this set on the line the tests follow.
    fn listed(&self) -> Candidate {
        self.listed_as(SLOT, IMPLEMENTATION)
    }

    fn listed_as(&self, slot: &str, implementation: &str) -> Candidate {
        Candidate {
            slot: slot.into(),
            implementation: implementation.into(),
            version: self.version.clone(),
            kind: CandidateKind::NativeSet,
            manifest_sha256: self.digest.clone(),
        }
    }
}

struct Publisher {
    signer: SigningKey,
    /// The directory the test connector serves as the repository location.
    directory: PathBuf,
}

impl Publisher {
    fn new(directory: PathBuf, seed: u8) -> Self {
        fs::create_dir_all(&directory).unwrap();
        Self {
            signer: SigningKey::from_bytes(&[seed; 32]),
            directory,
        }
    }

    fn public_key(&self) -> String {
        encode_hex(self.signer.verifying_key().as_bytes())
    }

    fn fingerprint(&self) -> String {
        sha256(self.signer.verifying_key().as_bytes())
    }

    fn approval(&self) -> ApprovedRepository {
        ApprovedRepository::approve(
            REPOSITORY.into(),
            "https://updates.example.test/amaterasu".into(),
            self.public_key(),
            self.fingerprint(),
        )
        .unwrap()
    }

    /// Signs a set whose slot version is its runtime module's version.
    fn sign_set(&self, sequence: u64, modules: &[Module]) -> SignedSet {
        self.sign_set_until(sequence, modules, unix_timestamp() + 3600)
    }

    /// The same, with a manifest that expires at `expires_unix`.
    fn sign_set_until(&self, sequence: u64, modules: &[Module], expires_unix: u64) -> SignedSet {
        let components = modules
            .iter()
            .map(|module| ComponentManifest {
                id: module.id.into(),
                version: module.version.into(),
                file: library_name(module.id),
                sha256: sha256(module.contents),
                size: module.contents.len() as u64,
                abi_major: amaterasu_module_sdk::abi::ABI_MAJOR,
                runtime: module.id == SLOT,
                requires: BTreeMap::new(),
                requested: Grants::default(),
            })
            .collect();
        let manifest = ReleaseManifest {
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            sequence,
            expires_unix,
            target: current_target(),
            components,
            privileged_requests: Vec::new(),
        };
        let raw = serde_json::to_vec(&manifest).unwrap();
        let signature = ReleaseSignature {
            key_id: self.fingerprint(),
            signature: encode_hex(&self.signer.sign(&raw).to_bytes()),
        };
        let mut files: BTreeMap<String, Vec<u8>> = modules
            .iter()
            .map(|module| (library_name(module.id), module.contents.to_vec()))
            .collect();
        files.insert(
            "manifest.sig".into(),
            serde_json::to_vec(&signature).unwrap(),
        );
        files.insert("manifest.json".into(), raw.clone());
        SignedSet {
            digest: sha256(&raw),
            version: modules
                .iter()
                .find(|module| module.id == SLOT)
                .expect("a set has a runtime")
                .version
                .into(),
            files,
        }
    }

    /// Serves a set's files where a catalog choice says they are.
    fn serve(&self, set: &SignedSet, implementation: &str) {
        set.write_to(&self.set_directory(set, implementation));
    }

    fn set_directory(&self, set: &SignedSet, implementation: &str) -> PathBuf {
        self.directory
            .join("native")
            .join(implementation)
            .join(&set.version)
    }

    /// Serves a signed catalog that lists `candidates`.
    fn publish_catalog(&self, sequence: u64, candidates: Vec<Candidate>) {
        self.publish_catalog_for(&current_target(), sequence, candidates);
    }

    /// Serves a signed catalog made for `target`.
    fn publish_catalog_for(&self, target: &str, sequence: u64, candidates: Vec<Candidate>) {
        let catalog = Catalog {
            schema: 1,
            repository: REPOSITORY.into(),
            sequence,
            expires_unix: unix_timestamp() + 3600,
            target: target.into(),
            candidates,
        };
        let raw = serde_json::to_vec(&catalog).unwrap();
        let mut signed = CATALOG_DOMAIN.to_vec();
        signed.extend_from_slice(&raw);
        let signature = encode_hex(&self.signer.sign(&signed).to_bytes());
        fs::write(self.directory.join("catalog.json"), raw).unwrap();
        fs::write(self.directory.join("catalog.sig"), signature).unwrap();
    }

    /// Serves `sets` on the followed line and a catalog that lists them.
    fn publish(&self, catalog_sequence: u64, sets: &[&SignedSet]) {
        for set in sets {
            self.serve(set, IMPLEMENTATION);
        }
        self.publish_catalog(
            catalog_sequence,
            sets.iter().map(|set| set.listed()).collect(),
        );
    }
}

/// Serves a directory as a repository. A held connector keeps its workers
/// waiting until it is released or they are cancelled, so a test can observe
/// work that is in flight. A stalled one does not notice cancellation until
/// it is released, like a provider call that has yet to return.
#[derive(Clone)]
struct DirectoryConnector {
    directory: PathBuf,
    held: Arc<AtomicBool>,
    stalled: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl DirectoryConnector {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            held: Arc::new(AtomicBool::new(false)),
            stalled: Arc::new(AtomicBool::new(false)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn hold(&self) {
        self.held.store(true, Ordering::Release);
    }

    fn stall(&self) {
        self.stalled.store(true, Ordering::Release);
    }

    fn release(&self) {
        self.held.store(false, Ordering::Release);
        self.stalled.store(false, Ordering::Release);
    }

    /// Every path requested so far, in order, and forgets them.
    fn take_requests(&self) -> Vec<String> {
        std::mem::take(&mut self.requests.lock().unwrap())
    }
}

struct DirectoryRepository {
    connector: DirectoryConnector,
    cancelled: Arc<AtomicBool>,
    transferred: Arc<AtomicU64>,
}

impl Connector for DirectoryConnector {
    type Transport = DirectoryRepository;

    fn connect(
        &self,
        location: &str,
        cancelled: Arc<AtomicBool>,
        transferred: Arc<AtomicU64>,
        _request_limit: Duration,
    ) -> DirectoryRepository {
        assert_eq!(location, "https://updates.example.test/amaterasu");
        DirectoryRepository {
            connector: self.clone(),
            cancelled,
            transferred,
        }
    }
}

impl RepositoryTransport for DirectoryRepository {
    fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>> {
        let waiting = || {
            self.connector.stalled.load(Ordering::Acquire)
                || (self.connector.held.load(Ordering::Acquire)
                    && !self.cancelled.load(Ordering::Acquire))
        };
        while waiting() {
            thread::sleep(Duration::from_millis(1));
        }
        if self.cancelled.load(Ordering::Acquire) {
            return Err("repository request cancelled".into());
        }
        self.connector
            .requests
            .lock()
            .unwrap()
            .push(relative.into());
        let bytes =
            fs::read(self.connector.directory.join(relative)).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > maximum {
            return Err("repository response exceeds byte limit".into());
        }
        self.transferred
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(bytes)
    }
}

/// A production-shaped host root that runs one of the publisher's sets.
struct Host {
    root: PathBuf,
    publisher: Publisher,
    running: SignedSet,
    connector: DirectoryConnector,
}

impl Host {
    /// A host running `modules` at `sequence`, recorded as supplied by the
    /// publisher's repository.
    fn running(sequence: u64, modules: &[Module]) -> Self {
        let host = Self::installed(sequence, modules);
        host.record_provenance();
        host
    }

    /// The same host without the selection naming a supplier.
    fn installed(sequence: u64, modules: &[Module]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-package-update-{}",
            random_identifier().unwrap()
        ));
        fs::create_dir_all(root.join("data/state")).unwrap();
        let root = root.canonicalize().unwrap();
        let publisher = Publisher::new(root.join("served-repository"), 71);
        let running = publisher.sign_set(sequence, modules);
        running.write_to(&package_directory(&root, &running.digest).unwrap());
        let bootstrap = BootstrapConfig {
            development: false,
            allow_unsigned: false,
            trusted_keys: BTreeMap::from([(publisher.fingerprint(), publisher.public_key())]),
            policy: RuntimeConfig::default(),
        };
        write_json_atomically(&root.join("bootstrap.json"), &bootstrap).unwrap();
        let host = Self {
            connector: DirectoryConnector::new(publisher.directory.clone()),
            root,
            publisher,
            running,
        };
        host.store().approve(host.publisher.approval()).unwrap();
        host.write_selection(&ActivationState {
            active: Some(host.running.digest.clone()),
            highest_sequence: sequence,
            ..Default::default()
        });
        host
    }

    fn record_provenance(&self) {
        let mut selection = self.selection();
        selection.repository_sets.insert(
            self.running.digest.clone(),
            RepositorySet {
                repository_id: REPOSITORY.into(),
                key_fingerprint: self.publisher.fingerprint(),
            },
        );
        self.write_selection(&selection);
    }

    fn selection(&self) -> ActivationState {
        amaterasu_package::read_json(&self.root.join("selection.json")).unwrap()
    }

    fn write_selection(&self, selection: &ActivationState) {
        write_json_atomically(&self.root.join("selection.json"), selection).unwrap();
    }

    fn selection_revision(&self) -> String {
        sha256(&fs::read(self.root.join("selection.json")).unwrap())
    }

    fn bootstrap(&self) -> BootstrapConfig {
        amaterasu_package::read_json(&self.root.join("bootstrap.json")).unwrap()
    }

    fn repository_directory(&self) -> PathBuf {
        self.root.join("repositories")
    }

    fn store(&self) -> RepositoryStore {
        RepositoryStore::new(self.repository_directory()).unwrap()
    }

    /// What a module instance reads when the host starts it on this root.
    fn environment(&self) -> Environment {
        Environment::load(
            Some(self.repository_directory()),
            Some(self.root.clone()),
            Some(self.running.digest.clone()),
        )
    }

    fn service(&self) -> Service<DirectoryConnector> {
        Service::new(self.connector.clone(), self.environment())
    }

    /// Where the cache keeps `set` once it is prepared.
    fn cached(&self, set: &SignedSet) -> PathBuf {
        self.store()
            .existing_native_candidate_cache_path(REPOSITORY, &set.digest)
            .unwrap()
    }

    /// Every entry of this repository's candidate cache, finished or not.
    fn cache_entries(&self) -> Vec<String> {
        let directory = self
            .repository_directory()
            .join("native-candidates")
            .join(REPOSITORY);
        let Ok(entries) = fs::read_dir(directory) else {
            return Vec::new();
        };
        entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect()
    }

    /// Writes the receipt the host leaves after an activation attempt.
    fn write_receipt(&self, digest: &str, status: &str) {
        write_json_atomically(
            &self.root.join("data/state/package-activation-status.json"),
            &serde_json::json!({"schema": 1, "digest": digest, "status": status}),
        )
        .unwrap();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The host shell's activation endpoint, scripted by the test.
struct ScriptedHost {
    admission: Admission,
    state: Option<HostActivationState>,
    withdraws: bool,
    requests: Vec<HostPackageActivation>,
    withdrawals: usize,
    /// False for a host shell from before restore requests.
    restores: bool,
    restore_requests: Vec<HostPackageRestore>,
    /// How many times the service asked whether the host takes restores.
    support_queries: usize,
}

impl ScriptedHost {
    /// A host that accepts a request and holds it until streams end.
    fn accepting() -> Self {
        Self {
            admission: Admission::Accepted,
            state: None,
            withdraws: true,
            requests: Vec::new(),
            withdrawals: 0,
            restores: true,
            restore_requests: Vec::new(),
            support_queries: 0,
        }
    }

    fn answering(admission: Admission) -> Self {
        Self {
            admission,
            ..Self::accepting()
        }
    }

    fn report(&mut self, pending: bool, started: bool) {
        self.state = Some(HostActivationState { pending, started });
    }
}

impl ActivationHost for ScriptedHost {
    fn activate(&mut self, request: HostPackageActivation) -> Admission {
        self.requests.push(request);
        if self.admission == Admission::Accepted {
            self.report(true, false);
        }
        self.admission
    }

    fn restore(&mut self, request: HostPackageRestore) -> Admission {
        self.restore_requests.push(request);
        if self.admission == Admission::Accepted {
            self.report(true, false);
        }
        self.admission
    }

    fn restore_supported(&mut self) -> bool {
        self.support_queries += 1;
        self.restores
    }

    fn cancel(&mut self) -> bool {
        self.withdrawals += 1;
        if self.withdraws {
            self.report(false, false);
        }
        self.withdraws
    }

    fn state(&mut self) -> Option<HostActivationState> {
        self.state
    }
}

type TestService = Service<DirectoryConnector>;

fn ask(service: &mut TestService, host: &mut ScriptedHost, action: Action) -> Value {
    service.request(action, host, false)
}

/// Polls status until no worker is running, as the console does.
fn settle(service: &mut TestService, host: &mut ScriptedHost) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = ask(service, host, Action::Status);
        if !matches!(status["phase"].as_str(), Some("checking" | "downloading")) {
            return status;
        }
        assert!(Instant::now() < deadline, "worker did not finish: {status}");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Ticks at `now`, as the host does, until no worker is running: every
/// automatic step that follows from the one that is due has then been taken.
fn tick_to_rest(service: &mut TestService, host: &mut ScriptedHost, now: Instant) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        service.tick(now, host, false);
        if !service.has_worker() {
            return ask(service, host, Action::Status);
        }
        assert!(Instant::now() < deadline, "worker did not finish");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Starts `action` and waits for its worker.
fn run(service: &mut TestService, host: &mut ScriptedHost, action: Action) -> Value {
    let started = ask(service, host, action);
    assert_eq!(started["ok"], true, "{started}");
    settle(service, host)
}

fn unix_milliseconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn refusal(reply: &Value) -> &str {
    assert_eq!(reply["ok"], false, "{reply}");
    reply["code"].as_str().unwrap()
}
