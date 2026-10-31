//! Bounded host-owned log snapshots; browser identity must be injected by management.
use crate::{
    LogSnapshot, LoggerError, MAX_LOG_SNAPSHOT_CHUNK_BYTES, ProductLogger, map_send_error,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 16;
const SNAPSHOT_CAPACITY: usize = 8;
const SNAPSHOTS_PER_OWNER: usize = 2;
const SNAPSHOT_IDLE: Duration = Duration::from_secs(60);
const SNAPSHOT_WAIT: Duration = Duration::from_secs(2);
const EXPORT_CHUNK_BYTES: usize = 8192;

type Job = (u64, Value);

/// Runs log reads away from the host tick. Its input is an internal broker
/// request, not a browser payload; management must attach verified ownership.
pub struct HostLogService {
    jobs: SyncSender<Job>,
    completed: Receiver<Job>,
}

impl HostLogService {
    pub fn start(logger: ProductLogger) -> io::Result<Self> {
        let (jobs, incoming) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (results, completed) = mpsc::sync_channel(QUEUE_CAPACITY);
        thread::Builder::new()
            .name("amaterasu-host-log-reader".into())
            .spawn(move || {
                let mut store = SnapshotStore::default();
                loop {
                    match incoming.recv_timeout(Duration::from_millis(250)) {
                        Ok((request, payload)) => {
                            let response = store.handle(&logger, payload, Instant::now());
                            let _ = results.try_send((request, response));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => store.expire(Instant::now()),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self { jobs, completed })
    }

    pub fn submit(&self, request: u64, payload: Value) -> Result<(), LoggerError> {
        self.jobs
            .try_send((request, payload))
            .map_err(map_send_error)
    }

    pub fn try_complete(&self) -> Result<Option<Job>, LoggerError> {
        match self.completed.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(LoggerError::WorkerUnavailable),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Open {
        owner: String,
    },
    ExportStart {
        owner: String,
    },
    ExportStatus {
        owner: String,
        snapshot: String,
    },
    ExportRead {
        owner: String,
        snapshot: String,
        offset: u64,
        limit: usize,
    },
    Read {
        owner: String,
        snapshot: String,
        offset: u64,
        limit: usize,
    },
    Close {
        owner: String,
        snapshot: String,
    },
}

struct Entry {
    owner: String,
    snapshot: LogSnapshot,
    last_access: Instant,
}

struct ExportEntry {
    owner: String,
    state: ExportState,
    last_access: Instant,
}

enum ExportState {
    Building(Receiver<io::Result<crate::support_logs::SupportBundle>>),
    Ready(crate::support_logs::SupportBundle),
    Failed,
}

struct ExportWorkerLease(Arc<AtomicBool>);
impl Drop for ExportWorkerLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Default)]
struct SnapshotStore {
    entries: BTreeMap<String, Entry>,
    exports: BTreeMap<String, ExportEntry>,
    export_active: Arc<AtomicBool>,
}

impl SnapshotStore {
    fn handle(&mut self, logger: &ProductLogger, payload: Value, now: Instant) -> Value {
        self.expire(now);
        let Ok(operation) = serde_json::from_value::<Operation>(payload) else {
            return failure("invalid_request");
        };
        match operation {
            Operation::Open { owner } if valid_owner(&owner) => self.open(logger, owner, now),
            Operation::ExportStart { owner } if valid_owner(&owner) => {
                self.start_export(logger, owner, now)
            }
            Operation::ExportStatus { owner, snapshot } if valid_owner(&owner) => {
                self.export_status(&owner, &snapshot, now)
            }
            Operation::ExportRead {
                owner,
                snapshot,
                offset,
                limit,
            } if valid_owner(&owner) => self.export_read(&owner, &snapshot, offset, limit, now),
            Operation::Read {
                owner,
                snapshot,
                offset,
                limit,
            } if valid_owner(&owner) => self.read(&owner, &snapshot, offset, limit, now),
            Operation::Close { owner, snapshot } if valid_owner(&owner) => {
                self.close(&owner, &snapshot)
            }
            _ => failure("invalid_request"),
        }
    }

    fn expire(&mut self, now: Instant) {
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.last_access) < SNAPSHOT_IDLE);
        self.exports
            .retain(|_, entry| now.saturating_duration_since(entry.last_access) < SNAPSHOT_IDLE);
    }

    fn start_export(&mut self, logger: &ProductLogger, owner: String, now: Instant) -> Value {
        if !self.exports.is_empty()
            || self
                .export_active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return failure("export_busy");
        }
        let Some(id) = (0..4).find_map(|_| {
            let id = random_id()?;
            (!self.entries.contains_key(&id) && !self.exports.contains_key(&id)).then_some(id)
        }) else {
            self.export_active.store(false, Ordering::Release);
            return failure("log_unavailable");
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let logger = logger.clone();
        let active = self.export_active.clone();
        let spawned = thread::Builder::new()
            .name("amaterasu-support-log-export".into())
            .spawn(move || {
                let _lease = ExportWorkerLease(active);
                let result = logger
                    .request_support_logs()
                    .map_err(|_| io::Error::other("log unavailable"))
                    .and_then(|reply| {
                        reply
                            .recv_timeout(SNAPSHOT_WAIT)
                            .map_err(|_| io::Error::other("log unavailable"))
                    })
                    .and_then(|source| source)
                    .and_then(crate::support_logs::build);
                let _ = sender.send(result);
            });
        if spawned.is_err() {
            self.export_active.store(false, Ordering::Release);
            return failure("log_unavailable");
        }
        self.exports.insert(
            id.clone(),
            ExportEntry {
                owner,
                state: ExportState::Building(receiver),
                last_access: now,
            },
        );
        json!({"ok":true,"snapshot":id,"status":"building"})
    }

    fn export_status(&mut self, owner: &str, id: &str, now: Instant) -> Value {
        if !valid_id(id) {
            return failure("invalid_request");
        }
        let Some(entry) = self
            .exports
            .get_mut(id)
            .filter(|entry| entry.owner == owner)
        else {
            return failure("not_found");
        };
        entry.last_access = now;
        if let ExportState::Building(receiver) = &entry.state {
            match receiver.try_recv() {
                Ok(Ok(bundle)) => entry.state = ExportState::Ready(bundle),
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => entry.state = ExportState::Failed,
                Err(TryRecvError::Empty) => {}
            }
        }
        match &entry.state {
            ExportState::Building(_) => json!({"ok":true,"snapshot":id,"status":"building"}),
            ExportState::Ready(bundle) => {
                json!({"ok":true,"snapshot":id,"status":"ready","length":bundle.bytes.len(),"sessions":bundle.sessions,"truncated":bundle.truncated})
            }
            ExportState::Failed => failure("log_unavailable"),
        }
    }

    fn open(&mut self, logger: &ProductLogger, owner: String, now: Instant) -> Value {
        if self.entries.len() >= SNAPSHOT_CAPACITY
            || self
                .entries
                .values()
                .filter(|entry| entry.owner == owner)
                .count()
                >= SNAPSHOTS_PER_OWNER
        {
            return failure("snapshot_limit");
        }
        let Some(id) = (0..4).find_map(|_| {
            let id = random_id()?;
            (!self.entries.contains_key(&id) && !self.exports.contains_key(&id)).then_some(id)
        }) else {
            return failure("log_unavailable");
        };
        let snapshot = match logger
            .request_snapshot()
            .ok()
            .and_then(|reply| reply.recv_timeout(SNAPSHOT_WAIT).ok())
        {
            Some(Ok(snapshot)) => snapshot,
            _ => return failure("log_unavailable"),
        };
        let length = snapshot.len();
        let discarded = snapshot.earlier_segments_discarded();
        let stream_id = snapshot.stream_id().to_owned();
        self.entries.insert(
            id.clone(),
            Entry {
                owner,
                snapshot,
                last_access: now,
            },
        );
        json!({"ok":true,"snapshot":id,"stream_id":stream_id,"length":length,"earlier_segments_discarded":discarded})
    }

    fn read(&mut self, owner: &str, id: &str, offset: u64, limit: usize, now: Instant) -> Value {
        if limit == 0 || limit > MAX_LOG_SNAPSHOT_CHUNK_BYTES {
            return failure("invalid_request");
        }
        if !valid_id(id) {
            return failure("invalid_request");
        };
        if let Some(entry) = self
            .exports
            .get_mut(id)
            .filter(|entry| entry.owner == owner)
        {
            return read_export_entry(entry, id, offset, limit, now);
        }
        let Some(entry) = self
            .entries
            .get_mut(id)
            .filter(|entry| entry.owner == owner)
        else {
            return failure("not_found");
        };
        if offset > entry.snapshot.len() {
            return failure("invalid_request");
        }
        let bytes = match entry.snapshot.read_chunk(offset, limit) {
            Ok(bytes) => bytes,
            Err(_) => return failure("log_unavailable"),
        };
        entry.last_access = now;
        let next = offset + bytes.len() as u64;
        json!({"ok":true,"snapshot":id,"offset":offset,"next_offset":next,
            "length":entry.snapshot.len(),"data":STANDARD.encode(bytes),"eof":next == entry.snapshot.len()})
    }

    fn export_read(
        &mut self,
        owner: &str,
        id: &str,
        offset: u64,
        limit: usize,
        now: Instant,
    ) -> Value {
        if !valid_id(id) || limit == 0 || limit > EXPORT_CHUNK_BYTES {
            return failure("invalid_request");
        }
        let Some(entry) = self
            .exports
            .get_mut(id)
            .filter(|entry| entry.owner == owner)
        else {
            return failure("not_found");
        };
        read_export_entry(entry, id, offset, limit, now)
    }

    fn close(&mut self, owner: &str, id: &str) -> Value {
        if !valid_id(id) {
            return failure("invalid_request");
        };
        if self
            .exports
            .get(id)
            .is_some_and(|entry| entry.owner == owner)
        {
            self.exports.remove(id);
            return json!({"ok":true});
        }
        if !self
            .entries
            .get(id)
            .is_some_and(|entry| entry.owner == owner)
        {
            return failure("not_found");
        }
        self.entries.remove(id);
        json!({"ok":true})
    }
}

fn read_export_entry(
    entry: &mut ExportEntry,
    id: &str,
    offset: u64,
    limit: usize,
    now: Instant,
) -> Value {
    let ExportState::Ready(bundle) = &entry.state else {
        return failure("not_ready");
    };
    let Ok(offset) = usize::try_from(offset) else {
        return failure("invalid_request");
    };
    if offset > bundle.bytes.len() {
        return failure("invalid_request");
    }
    let next = offset.saturating_add(limit).min(bundle.bytes.len());
    entry.last_access = now;
    json!({"ok":true,"snapshot":id,"offset":offset,"next_offset":next,
        "length":bundle.bytes.len(),"data":STANDARD.encode(&bundle.bytes[offset..next]),"eof":next == bundle.bytes.len()})
}

fn valid_owner(owner: &str) -> bool {
    owner.len() == 64
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn random_id() -> Option<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).ok()?;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[usize::from(byte >> 4)] as char);
        result.push(HEX[usize::from(byte & 15)] as char);
    }
    Some(result)
}

fn failure(code: &str) -> Value {
    json!({"ok":false,"code":code})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LoggingPolicy;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "amaterasu-log-service-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn logger(directory: &TestDirectory) -> ProductLogger {
        let policy = LoggingPolicy::new(0, 2, "product.log", &directory.0).unwrap();
        ProductLogger::start(policy).unwrap()
    }

    #[test]
    fn snapshots_are_owned_bounded_and_return_exact_chunks() {
        let directory = TestDirectory::new();
        let logger = logger(&directory);
        logger.emit(2, "test", "first entry").unwrap();
        let mut store = SnapshotStore::default();
        let now = Instant::now();
        let owner = "a".repeat(64);
        let other = "b".repeat(64);
        let opened = store.handle(&logger, json!({"action":"open","owner":owner}), now);
        assert_eq!(opened["ok"], true);
        assert!(valid_id(opened["stream_id"].as_str().unwrap()));
        let stream_id = opened["stream_id"].clone();
        let id = opened["snapshot"].as_str().unwrap();
        assert!(valid_id(id));
        let length = opened["length"].as_u64().unwrap();
        let chunk = store.handle(
            &logger,
            json!({"action":"read","owner":owner,"snapshot":id,"offset":0,"limit":4096}),
            now,
        );
        assert_eq!(chunk["ok"], true);
        assert_eq!(chunk["next_offset"], length);
        assert_eq!(chunk["eof"], true);
        let bytes = STANDARD.decode(chunk["data"].as_str().unwrap()).unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("first entry"));
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"read","owner":other,"snapshot":id,"offset":0,"limit":1}),
                now
            )["code"],
            "not_found"
        );
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"close","owner":other,"snapshot":id}),
                now
            )["code"],
            "not_found"
        );
        for payload in [
            json!({"action":"open","owner":owner,"path":"/etc/passwd"}),
            json!({"action":"read","owner":owner,"snapshot":id,"offset":0,"limit":4097}),
            json!({"action":"read","owner":owner,"snapshot":id,"offset":length + 1,"limit":1}),
            json!({"action":"read","owner":owner,"snapshot":"../secret","offset":0,"limit":1}),
        ] {
            assert_eq!(
                store.handle(&logger, payload, now)["code"],
                "invalid_request"
            );
        }
        logger.emit(2, "test", "later entry").unwrap();
        let again = store.handle(
            &logger,
            json!({"action":"read","owner":owner,"snapshot":id,"offset":0,"limit":4096}),
            now,
        );
        assert_eq!(again["data"], chunk["data"]);
        let later = store.handle(&logger, json!({"action":"open","owner":owner}), now);
        assert_eq!(later["stream_id"], stream_id);
        assert_ne!(later["snapshot"], opened["snapshot"]);
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"close","owner":owner,"snapshot":id}),
                now
            )["ok"],
            true
        );
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"read","owner":owner,"snapshot":id,"offset":0,"limit":1}),
                now
            )["code"],
            "not_found"
        );
    }

    #[test]
    fn snapshot_limits_and_expiry_release_file_handles() {
        let directory = TestDirectory::new();
        let logger = logger(&directory);
        let mut store = SnapshotStore::default();
        let now = Instant::now();
        let owner = "a".repeat(64);
        for _ in 0..SNAPSHOTS_PER_OWNER {
            assert_eq!(
                store.handle(&logger, json!({"action":"open","owner":owner}), now)["ok"],
                true
            );
        }
        assert_eq!(
            store.handle(&logger, json!({"action":"open","owner":owner}), now)["code"],
            "snapshot_limit"
        );
        for ch in ["b", "c", "d"] {
            let other = ch.repeat(64);
            for _ in 0..SNAPSHOTS_PER_OWNER {
                assert_eq!(
                    store.handle(&logger, json!({"action":"open","owner":other}), now)["ok"],
                    true
                );
            }
        }
        assert_eq!(store.entries.len(), SNAPSHOT_CAPACITY);
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"open","owner":"e".repeat(64)}),
                now
            )["code"],
            "snapshot_limit"
        );
        store.expire(now + SNAPSHOT_IDLE);
        assert!(store.entries.is_empty());
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"open","owner":owner}),
                now + SNAPSHOT_IDLE
            )["ok"],
            true
        );
    }

    #[test]
    fn support_export_is_asynchronous_scoped_bounded_and_releasable() {
        let directory = TestDirectory::new();
        let logger = logger(&directory);
        logger
            .emit(2, "test", "support username=alice ip=192.168.1.9")
            .unwrap();
        let mut store = SnapshotStore::default();
        let owner = "a".repeat(64);
        let other = "b".repeat(64);
        let now = Instant::now();
        let opened = store.handle(&logger, json!({"action":"export_start","owner":owner}), now);
        assert_eq!(opened["status"], "building");
        let id = opened["snapshot"].as_str().unwrap();
        assert_eq!(
            store.handle(&logger, json!({"action":"export_start","owner":owner}), now)["code"],
            "export_busy"
        );
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"export_status","owner":other,"snapshot":id}),
                now
            )["code"],
            "not_found"
        );
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"export_read","owner":other,"snapshot":id,"offset":0,"limit":8192}),
                now
            )["code"],
            "not_found"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            let status = store.handle(
                &logger,
                json!({"action":"export_status","owner":owner,"snapshot":id}),
                Instant::now(),
            );
            if status["status"] == "ready" {
                break status;
            }
            assert_eq!(status["status"], "building");
            assert!(Instant::now() < deadline);
            thread::yield_now();
        };
        assert_eq!(status["sessions"], 1);
        let chunk = store.handle(
            &logger,
            json!({"action":"export_read","owner":owner,"snapshot":id,"offset":0,"limit":8192}),
            Instant::now(),
        );
        assert_eq!(chunk["ok"], true);
        let bytes = STANDARD.decode(chunk["data"].as_str().unwrap()).unwrap();
        assert!(bytes.starts_with(b"PK\x03\x04"));
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"export_read","owner":owner,"snapshot":id,"offset":0,"limit":8193}),
                now
            )["code"],
            "invalid_request"
        );
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"close","owner":other,"snapshot":id}),
                now
            )["code"],
            "not_found"
        );
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"close","owner":owner,"snapshot":id}),
                now
            )["ok"],
            true
        );
        assert_eq!(
            store.handle(
                &logger,
                json!({"action":"export_status","owner":owner,"snapshot":id}),
                now
            )["code"],
            "not_found"
        );
    }

    #[test]
    fn support_export_chunks_fit_the_broker_payload_limit() {
        let directory = TestDirectory::new();
        let logger = logger(&directory);
        let mut store = SnapshotStore::default();
        let owner = "a".repeat(64);
        let id = "b".repeat(32);
        store.exports.insert(
            id.clone(),
            ExportEntry {
                owner: owner.clone(),
                state: ExportState::Ready(crate::support_logs::SupportBundle {
                    bytes: vec![0x42; EXPORT_CHUNK_BYTES + 1],
                    sessions: 1,
                    truncated: false,
                }),
                last_access: Instant::now(),
            },
        );
        let chunk = store.handle(&logger, json!({"action":"export_read","owner":owner,"snapshot":id,"offset":0,"limit":EXPORT_CHUNK_BYTES}), Instant::now());
        assert_eq!(
            STANDARD
                .decode(chunk["data"].as_str().unwrap())
                .unwrap()
                .len(),
            EXPORT_CHUNK_BYTES
        );
        assert!(serde_json::to_vec(&chunk).unwrap().len() <= 16_384);
        assert_eq!(chunk["eof"], false);
    }

    #[test]
    fn queued_host_service_returns_results_without_blocking_the_submitter() {
        let directory = TestDirectory::new();
        let logger = logger(&directory);
        logger.emit(2, "test", "queued entry").unwrap();
        let service = HostLogService::start(logger).unwrap();
        service
            .submit(17, json!({"action":"open","owner":"a".repeat(64)}))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some((request, response)) = service.try_complete().unwrap() {
                assert_eq!(request, 17);
                assert_eq!(response["ok"], true);
                break;
            }
            assert!(Instant::now() < deadline, "host log worker did not reply");
            thread::yield_now();
        }
    }
}
