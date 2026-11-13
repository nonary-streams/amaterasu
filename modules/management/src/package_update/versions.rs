//! The retained sets a host can return to.
//!
//! The host keeps every set it ran healthy. This lists the newest of them for
//! the console and says which the host would accept a return to. The answer
//! is advice: the host shell verifies a set again, under its own protected
//! authority, before it runs one.
//!
//! Reading a host's sets means reading one manifest for each, so it happens
//! on a worker thread. A request is answered from what was last read.
use serde_json::{Value, json};

#[cfg(target_os = "linux")]
pub(crate) use listing::Versions;

/// The reply of a host that cannot return to a retained set.
pub(crate) fn unsupported() -> Value {
    json!({"ok": true, "supported": false, "total": 0, "versions": []})
}

/// A console request about retained sets, as the route table names it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VersionRequest {
    List,
    Restore { digest: String },
}

impl VersionRequest {
    pub(crate) const LIST: &'static str = "package_update_versions";
    pub(crate) const RESTORE: &'static str = "package_update_restore";

    /// True for the action names `routes::resolve` gives these requests.
    pub(crate) fn names(action: &str) -> bool {
        action == Self::LIST || action == Self::RESTORE
    }

    /// Reads a request that carries exactly what its route allows: nothing
    /// for a listing, and one digest for a restore.
    pub(crate) fn parse(payload: &Value) -> Option<Self> {
        let fields = payload.as_object()?;
        match (fields.get("action")?.as_str()?, fields.len()) {
            (Self::LIST, 1) => Some(Self::List),
            (Self::RESTORE, 2) => Some(Self::Restore {
                digest: fields.get("digest")?.as_str()?.to_owned(),
            }),
            _ => None,
        }
    }
}

/// Reading and listing the sets. Only a Linux host can return to one.
#[cfg(target_os = "linux")]
mod listing {
    use super::super::environment::Environment;
    use super::unsupported;
    use crate::package_status;
    use amaterasu_package::ReleaseManifest;
    use amaterasu_package::repository::RepositoryStore;
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        thread::{self, JoinHandle},
        time::{Duration, Instant, UNIX_EPOCH},
    };

    /// How many sets a reply lists. The newest come first and the running
    /// set is always among them.
    pub(crate) const LISTED_SETS: usize = 24;
    /// A listing older than this is read again when the console next asks.
    const REFRESH_AFTER: Duration = Duration::from_secs(30);
    const SELECTION_BYTES: u64 = 1024 * 1024;
    const BOOTSTRAP_BYTES: u64 = 1024 * 1024;

    /// How the selection records a listed set.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum State {
        Running,
        Previous,
        Retained,
    }

    impl State {
        fn name(self) -> &'static str {
            match self {
                Self::Running => "running",
                Self::Previous => "previous",
                Self::Retained => "retained",
            }
        }
    }

    /// One retained set, as last read.
    #[derive(Clone, Debug)]
    struct Entry {
        digest: String,
        sequence: u64,
        state: State,
        /// When the host wrote the set's manifest into its set store.
        installed_unix_ms: Option<u64>,
        modules: usize,
        /// Modules whose library differs from the running set's, with the ones
        /// only one of the two sets has.
        differs: usize,
        expires_unix: u64,
        /// Why the host would refuse the set whatever the time, when it would.
        blocked: Option<&'static str>,
    }

    impl Entry {
        /// Why the set cannot be returned to at `now`, or None when it can.
        fn refusal(&self, now: u64) -> Option<&'static str> {
            self.blocked
                .or_else(|| (self.expires_unix <= now).then_some("expired"))
        }

        fn restorable(&self, now: u64) -> bool {
            self.state != State::Running && self.refusal(now).is_none()
        }

        fn describe(&self, now: u64) -> Value {
            let running = self.state == State::Running;
            json!({
                "digest": self.digest,
                "sequence": self.sequence.to_string(),
                "state": self.state.name(),
                "installed_unix_ms": self.installed_unix_ms,
                "modules": self.modules,
                "differs": self.differs,
                "restorable": self.restorable(now),
                "reason": if running { None } else { self.refusal(now) },
            })
        }
    }

    /// A set the console may ask the host to return to.
    pub(crate) struct Target {
        pub digest: String,
        pub sequence: u64,
    }

    struct Listing {
        /// Every retained healthy set on disk, listed or not.
        total: usize,
        /// Newest first.
        entries: Vec<Entry>,
    }

    /// Where the worker reads from. Both directories come from startup composition.
    #[derive(Clone)]
    struct Source {
        package_root: PathBuf,
        repository_directory: Option<PathBuf>,
        running: String,
    }

    pub(crate) struct Versions {
        source: Option<Source>,
        listing: Option<Listing>,
        read_at: Option<Instant>,
        worker: Option<JoinHandle<Option<Listing>>>,
    }

    impl Versions {
        /// Starts the first read, so a listing is ready when the console asks.
        pub(crate) fn start(environment: &Environment) -> Self {
            let source = environment
                .package_root
                .clone()
                .zip(environment.running_digest.clone())
                .map(|(package_root, running)| Source {
                    package_root,
                    repository_directory: environment.repository_directory.clone(),
                    running,
                });
            let mut versions = Self {
                source,
                listing: None,
                read_at: None,
                worker: None,
            };
            versions.begin_read();
            versions
        }

        /// Retires a finished read without waiting for a running one.
        pub(crate) fn advance(&mut self) {
            if !self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                return;
            }
            let read = self.worker.take().and_then(|worker| worker.join().ok());
            // A read that failed keeps what the last one found.
            if let Some(Some(listing)) = read {
                self.listing = Some(listing);
            }
            self.read_at = Some(Instant::now());
        }

        /// The listing as last read. Asking also starts a fresh read when that
        /// one has aged, for the next request to find.
        pub(crate) fn reply(&mut self, now: u64) -> Value {
            self.advance();
            if self
                .read_at
                .is_some_and(|read_at| read_at.elapsed() >= REFRESH_AFTER)
            {
                self.begin_read();
            }
            match &self.listing {
                Some(listing) => json!({
                    "ok": true,
                    "supported": true,
                    "total": listing.total,
                    "versions": listing
                        .entries
                        .iter()
                        .map(|entry| entry.describe(now))
                        .collect::<Vec<_>>(),
                }),
                // Only a request that beats the module's first read sees this.
                None if self.worker.is_some() => json!({
                    "ok": true,
                    "supported": true,
                    "total": 0,
                    "versions": [],
                    "loading": true,
                }),
                // The host root or the running set could not be read.
                None => unsupported(),
            }
        }

        /// True until the first read has finished.
        pub(crate) fn loading(&mut self) -> bool {
            self.advance();
            self.listing.is_none() && self.worker.is_some()
        }

        /// True while a read's worker thread has yet to be retired.
        pub(crate) fn reading(&self) -> bool {
            self.worker.is_some()
        }

        /// The listed set with this digest, when the host would accept a return
        /// to it at `now`.
        pub(crate) fn restorable(&mut self, digest: &str, now: u64) -> Option<Target> {
            self.advance();
            self.listing
                .as_ref()?
                .entries
                .iter()
                .find(|entry| entry.digest == digest && entry.restorable(now))
                .map(|entry| Target {
                    digest: entry.digest.clone(),
                    sequence: entry.sequence,
                })
        }

        fn begin_read(&mut self) {
            if self.worker.is_some() {
                return;
            }
            let Some(source) = self.source.clone() else {
                return;
            };
            self.read_at = None;
            self.worker = thread::Builder::new()
                .name("amaterasu-package-versions".into())
                .spawn(move || read(&source))
                .ok();
            if self.worker.is_none() {
                // Try again when the console next asks.
                self.read_at = Some(Instant::now());
            }
        }
    }

    #[cfg(test)]
    impl Versions {
        /// Starts a read now and waits for it, whatever the listing's age.
        pub(crate) fn reread(&mut self) {
            while self.worker.is_some() {
                thread::yield_now();
                self.advance();
            }
            self.begin_read();
            while self.worker.is_some() {
                thread::yield_now();
                self.advance();
            }
        }
    }

    impl Drop for Versions {
        fn drop(&mut self) {
            // The read is bounded: a selection, a bootstrap and one manifest per set.
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    /// A retained set's manifest and when the host stored it.
    struct Retained {
        digest: String,
        manifest: ReleaseManifest,
        installed_unix_ms: Option<u64>,
    }

    /// Runs on a worker thread. None when the host root cannot be read at all.
    fn read(source: &Source) -> Option<Listing> {
        let root = package_status::trusted_root(source.package_root.to_str()?)?;
        let selection: amaterasu_package::ActivationState = serde_json::from_slice(
            &amaterasu_package::read_bounded(&root.join("selection.json"), SELECTION_BYTES).ok()?,
        )
        .ok()?;
        let running = package_status::running_manifest(root, &source.running)?;
        let running_libraries = libraries(&running);

        let mut retained: Vec<Retained> = selection
            .healthy_sets
            .iter()
            .filter(|digest| **digest != source.running && package_status::is_sha256(digest))
            .filter_map(|digest| {
                Some(Retained {
                    manifest: package_status::running_manifest(root, digest)?,
                    installed_unix_ms: stored_unix_ms(root, digest),
                    digest: digest.clone(),
                })
            })
            .collect();
        retained.push(Retained {
            installed_unix_ms: stored_unix_ms(root, &source.running),
            digest: source.running.clone(),
            manifest: running,
        });
        let total = retained.len();
        retained.sort_by(|a, b| {
            b.manifest
                .sequence
                .cmp(&a.manifest.sequence)
                .then_with(|| a.digest.cmp(&b.digest))
        });
        // The running set is always listed, in its place among the others.
        if let Some(position) = retained
            .iter()
            .position(|set| set.digest == source.running)
            .filter(|position| *position >= LISTED_SETS)
        {
            let running = retained.remove(position);
            retained.truncate(LISTED_SETS - 1);
            retained.push(running);
        }
        retained.truncate(LISTED_SETS);

        let publishers = Publishers::read(root, source, &selection);
        let entries = retained
            .into_iter()
            .map(|set| {
                let state = if set.digest == source.running {
                    State::Running
                } else if selection.previous.as_deref() == Some(set.digest.as_str()) {
                    State::Previous
                } else {
                    State::Retained
                };
                Entry {
                    blocked: publishers.blocked(&selection, &set),
                    differs: differing(&running_libraries, &libraries(&set.manifest)),
                    modules: set.manifest.components.len(),
                    expires_unix: set.manifest.expires_unix,
                    sequence: set.manifest.sequence,
                    installed_unix_ms: set.installed_unix_ms,
                    digest: set.digest,
                    state,
                }
            })
            .collect();
        Some(Listing { total, entries })
    }

    /// Each module's library digest, by module identity.
    fn libraries(manifest: &ReleaseManifest) -> BTreeMap<String, String> {
        manifest
            .components
            .iter()
            .map(|component| (component.id.clone(), component.sha256.clone()))
            .collect()
    }

    /// How many modules two sets do not share: a different library, or a module
    /// only one of them has.
    fn differing(running: &BTreeMap<String, String>, other: &BTreeMap<String, String>) -> usize {
        let changed_or_removed = running
            .iter()
            .filter(|(id, library)| other.get(*id) != Some(*library))
            .count();
        let added = other.keys().filter(|id| !running.contains_key(*id)).count();
        changed_or_removed + added
    }

    fn stored_unix_ms(root: &Path, digest: &str) -> Option<u64> {
        let manifest = amaterasu_package::package_directory(root, digest)
            .ok()?
            .join("manifest.json");
        let stored = std::fs::metadata(manifest).ok()?.modified().ok()?;
        u64::try_from(stored.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
    }

    /// What decides whether a retained set's publisher is still one the host
    /// would run: the running set's own publisher, the live approvals and the
    /// keys the bootstrap trusts.
    struct Publishers {
        /// The key the running set was accepted under, when the selection says.
        running: Option<String>,
        store: Option<RepositoryStore>,
        trusted_keys: BTreeMap<String, String>,
    }

    impl Publishers {
        fn read(
            root: &Path,
            source: &Source,
            selection: &amaterasu_package::ActivationState,
        ) -> Self {
            let trusted_keys =
                amaterasu_package::read_bounded(&root.join("bootstrap.json"), BOOTSTRAP_BYTES)
                    .ok()
                    .and_then(|bytes| {
                        serde_json::from_slice::<amaterasu_package::BootstrapConfig>(&bytes).ok()
                    })
                    .map(|bootstrap| bootstrap.trusted_keys)
                    .unwrap_or_default();
            Self {
                running: selection
                    .repository_sets
                    .get(&source.running)
                    .map(|supplier| supplier.key_fingerprint.clone()),
                store: source
                    .repository_directory
                    .clone()
                    .and_then(|directory| RepositoryStore::new(directory).ok()),
                trusted_keys,
            }
        }

        /// Why the host would refuse `set` whatever the time, when it would.
        fn blocked(
            &self,
            selection: &amaterasu_package::ActivationState,
            set: &Retained,
        ) -> Option<&'static str> {
            let Some(supplier) = selection.repository_sets.get(&set.digest) else {
                // The host cannot tell who published a set it has no record for.
                return Some("unavailable");
            };
            if !package_status::is_repository_id(&supplier.repository_id)
                || !package_status::is_sha256(&supplier.key_fingerprint)
            {
                return Some("unavailable");
            }
            let trusted = self
                .running
                .as_ref()
                .is_none_or(|running| *running == supplier.key_fingerprint)
                && self
                    .store
                    .as_ref()
                    .and_then(|store| store.approved(&supplier.repository_id).ok())
                    .is_some_and(|approved| {
                        approved.fingerprint == supplier.key_fingerprint
                            && self.trusted_keys.get(&approved.fingerprint)
                                == Some(&approved.public_key)
                    });
            if !trusted {
                return Some("publisher_untrusted");
            }
            // A set above its publisher's accepted release, or another set at
            // that release's number, was never what the host accepted.
            let within_accepted_release = selection
                .repository_floors
                .get(&supplier.key_fingerprint)
                .is_some_and(|floor| {
                    set.manifest.sequence < floor.sequence
                        || (set.manifest.sequence == floor.sequence && set.digest == floor.digest)
                });
            (!within_accepted_release || set.manifest.host_context_schema != 1)
                .then_some("unavailable")
        }
    }
}
