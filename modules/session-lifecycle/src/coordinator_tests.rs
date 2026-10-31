use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-lifecycle-coordinator-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }

    fn journal(&self) -> std::path::PathBuf {
        self.0.join("journal.json")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn admission_is_durable_idempotent_and_recovered_without_replaying_work() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    let request = json!({"operation":"prepare", "operation_id":"op-1"});
    let first = coordinator.handle("transport", "client", request.clone());
    assert_eq!(first["status"], "pending");
    assert_eq!(
        coordinator.handle("transport", "client", request.clone()),
        first
    );
    let old_epoch = first["host_epoch"].clone();
    drop(coordinator);

    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    let recovered = coordinator.handle("transport", "client", request);
    assert_eq!(recovered["status"], "recovery_required");
    assert_eq!(recovered["reason"], "coordinator_restarted");
    assert_eq!(recovered["session_id"], first["session_id"]);
    assert_eq!(recovered["transition_id"], first["transition_id"]);
    assert_ne!(
        coordinator.handle("transport", "client", json!({"operation":"snapshot"}))["host_epoch"],
        old_epoch
    );
}

#[test]
fn lookup_and_replay_are_bound_to_broker_caller_initiator_and_request() {
    let dir = TestDirectory::new();
    let mut coordinator = Coordinator::open(&dir.journal()).unwrap();
    let request = json!({"operation":"prepare", "operation_id":"op-1"});
    let receipt = coordinator.handle("transport", "client", request.clone());
    let lookup = json!({"operation":"lookup", "operation_id":"op-1"});
    assert_eq!(
        coordinator.handle("transport", "client", lookup.clone()),
        receipt
    );
    assert_eq!(
        coordinator.handle("other", "client", lookup.clone())["status"],
        "not_found"
    );
    assert_eq!(
        coordinator.handle("transport", "other", lookup)["status"],
        "not_found"
    );
    assert_eq!(
        coordinator.handle("other", "client", request.clone())["status"],
        "pending"
    );
    assert_eq!(
        coordinator.handle("transport", "other", request)["status"],
        "pending"
    );
    assert_eq!(
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"prepare", "operation_id":"op-1", "extra":true})
        )["reason"],
        "operation_id_conflict"
    );
}

#[test]
fn failed_journal_replacement_never_exposes_a_pending_receipt() {
    let dir = TestDirectory::new();
    let path = dir.journal();
    let mut coordinator = Coordinator::open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let request = json!({"operation":"prepare", "operation_id":"op-1"});
    let failed = coordinator.handle("transport", "client", request.clone());
    assert_eq!(failed["status"], "recovery_required");
    assert_eq!(failed["reason"], "journal_write_failed");
    assert_eq!(
        coordinator.handle(
            "transport",
            "client",
            json!({"operation":"lookup", "operation_id":"op-1"})
        )["status"],
        "not_found"
    );
    std::fs::remove_dir(&path).unwrap();
    assert_eq!(
        coordinator.handle("transport", "client", request)["status"],
        "recovery_required"
    );
}

#[test]
fn semantically_corrupt_journal_is_rejected_before_recovery() {
    let dir = TestDirectory::new();
    let path = dir.journal();
    let mut coordinator = Coordinator::open(&path).unwrap();
    let request = json!({"operation":"prepare", "operation_id":"op-1"});
    assert_eq!(
        coordinator.handle("transport", "client", request)["status"],
        "pending"
    );
    drop(coordinator);
    let mut journal: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    journal["receipts"][receipt_key("transport", "client", "op-1")]["response"]["operation_id"] =
        json!("different-operation");
    std::fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
    assert!(
        matches!(Coordinator::open(&path), Err(error) if error.kind() == io::ErrorKind::InvalidData)
    );
}
