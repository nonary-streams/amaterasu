//! How the update workers reach a repository, and which bytes they need not
//! fetch because the running set already holds them.
use super::environment::RunningSet;
use super::plan::{Offer, held_by, payloads};
use crate::repository_transport::NetworkRepository;
use amaterasu_module_sdk::network::Network;
use amaterasu_package::PackageResult;
use amaterasu_package::repository::RepositoryTransport;
use std::{
    cell::Cell,
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
    time::Duration,
};

/// Opens a transport to an approved repository location. The production
/// connector is the module's Network SDK provider; tests supply a directory.
pub(crate) trait Connector: Clone + Send + 'static {
    type Transport: RepositoryTransport;

    /// `request_limit` bounds one request. The transport must stop when
    /// `cancelled` is set and count the bytes it receives in `transferred`.
    fn connect(
        &self,
        location: &str,
        cancelled: Arc<AtomicBool>,
        transferred: Arc<AtomicU64>,
        request_limit: Duration,
    ) -> Self::Transport;
}

impl Connector for Network {
    type Transport = NetworkRepository;

    fn connect(
        &self,
        location: &str,
        cancelled: Arc<AtomicBool>,
        transferred: Arc<AtomicU64>,
        request_limit: Duration,
    ) -> NetworkRepository {
        NetworkRepository::new(
            location,
            self.clone(),
            cancelled,
            transferred,
            request_limit,
        )
    }
}

/// Remembers whether the repository failed to answer, so a worker can tell an
/// unreachable repository from bytes that did not verify.
pub(crate) struct Observed<T> {
    remote: T,
    failed: Cell<bool>,
}

impl<T> Observed<T> {
    pub(crate) fn new(remote: T) -> Self {
        Self {
            remote,
            failed: Cell::new(false),
        }
    }

    /// True once any request to the repository has failed.
    pub(crate) fn failed(&self) -> bool {
        self.failed.get()
    }
}

impl<T: RepositoryTransport> RepositoryTransport for Observed<T> {
    fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>> {
        self.remote
            .get(relative, maximum)
            .inspect_err(|_| self.failed.set(true))
    }
}

/// A file of the running set that holds the bytes an offered set signs.
pub(crate) struct HeldFile {
    path: PathBuf,
    sha256: String,
    size: u64,
}

impl HeldFile {
    /// The file's bytes, only when they are exactly the signed bytes.
    fn read(&self, maximum: u64) -> Option<Vec<u8>> {
        if self.size > maximum {
            return None;
        }
        let bytes = amaterasu_package::read_bounded(&self.path, self.size).ok()?;
        (bytes.len() as u64 == self.size && amaterasu_package::sha256(&bytes) == self.sha256)
            .then_some(bytes)
    }
}

/// The offered set's files the running set already holds, keyed by the
/// offered file name.
///
/// Reuse is an optimization only: the downloaded candidate is verified as a
/// whole exactly as if every byte had come from the repository.
pub(crate) fn held_files(running: &RunningSet, offer: &Offer) -> BTreeMap<String, HeldFile> {
    payloads(&offer.manifest)
        .filter_map(|offered| {
            let held = held_by(&running.manifest, offered)?;
            if !is_plain_filename(held.file) {
                return None;
            }
            Some((
                offered.file.to_owned(),
                HeldFile {
                    path: running.directory.join(held.file),
                    sha256: offered.sha256.to_owned(),
                    size: offered.size,
                },
            ))
        })
        .collect()
}

/// A name that stays inside the running set's directory.
fn is_plain_filename(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

/// Answers a request from the running set when it holds the exact bytes, and
/// from the repository otherwise.
pub(crate) struct Reusing<'a, T> {
    remote: &'a Observed<T>,
    held: BTreeMap<String, HeldFile>,
}

impl<'a, T> Reusing<'a, T> {
    pub(crate) fn new(remote: &'a Observed<T>, held: BTreeMap<String, HeldFile>) -> Self {
        Self { remote, held }
    }
}

impl<T: RepositoryTransport> RepositoryTransport for Reusing<'_, T> {
    fn get(&self, relative: &str, maximum: u64) -> PackageResult<Vec<u8>> {
        // A set's files sit side by side, so the last segment names the file.
        let held = relative
            .rsplit('/')
            .next()
            .and_then(|file| self.held.get(file))
            .and_then(|file| file.read(maximum));
        match held {
            Some(bytes) => Ok(bytes),
            None => self.remote.get(relative, maximum),
        }
    }
}
