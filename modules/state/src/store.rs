use crate::protocol::{Operation, StateError};
use crate::snapshot;
use amaterasu_contracts::{LiveFeedbackStateOrigin, LiveFeedbackStateOriginPhase};
use amaterasu_storage::{
    RootLock, create_private_directory, read_bounded, sync_directory, write_json_atomically,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_KEY_BYTES: usize = 256;
const MAX_VALUE_BYTES: usize = snapshot::MAX_RESPONSE_BYTES;
const MAX_NAMESPACE_BYTES: u64 = 1024 * 1024;
const MAX_KEYS: usize = 4096;
const HISTORY_FILE: &str = ".state-history-v1.json";
const STATE_ORIGIN_FILE: &str = ".live-feedback-state-origin-v1.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StateHistory {
    version: u8,
    phase: StateHistoryPhase,
    claim: Option<amaterasu_module_sdk::state::FeedbackBootstrapClaim>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StateHistoryPhase {
    Fresh,
    Used,
    BootstrapIntent,
    BootstrapCommitted,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Namespace {
    format_version: u32,
    revision: u64,
    values: BTreeMap<String, Value>,
}

impl Default for Namespace {
    fn default() -> Self {
        Self {
            format_version: 1,
            revision: 0,
            values: BTreeMap::new(),
        }
    }
}

pub(crate) struct StateStore {
    directory: PathBuf,
    // An exclusive lifetime lock prevents lost updates across module processes.
    _lock: RootLock,
}

impl StateStore {
    pub fn open(directory: PathBuf) -> Result<Self, String> {
        create_private_directory(&directory)?;
        let lock = RootLock::acquire(&directory)?;
        let store = Self {
            directory,
            _lock: lock,
        };
        store
            .ensure_history_record()
            .map_err(|error| error.to_string())?;
        Ok(store)
    }

    pub fn handle(&self, caller: &str, payload: Value) -> Value {
        let response = self
            .execute(caller, payload)
            .unwrap_or_else(StateError::payload);
        if snapshot::response_fits(&response) {
            response
        } else {
            StateError::new("response_too_large", "encoded response exceeds 16384 bytes").payload()
        }
    }

    fn execute(&self, caller: &str, payload: Value) -> Result<Value, StateError> {
        let operation: Operation = serde_json::from_value(payload)
            .map_err(|error| StateError::new("invalid_request", error.to_string()))?;
        validate_operation(&operation)?;
        if let Operation::InitializeFeedbackHost {
            claim,
            resume_existing,
        } = &operation
        {
            return self.initialize_feedback_host(caller, claim.clone(), *resume_existing);
        }
        self.record_state_use()?;
        let path = self.namespace_path(caller)?;
        match operation {
            Operation::InitializeFeedbackHost { .. } => unreachable!("handled above"),
            Operation::CollectionGet { collection, key } => {
                return crate::collection::Collection::open(&self.directory, &path, &collection)?
                    .get(key.as_deref());
            }
            Operation::CollectionPage {
                collection,
                revision,
                after,
                limit,
            } => {
                return crate::collection::Collection::open(&self.directory, &path, &collection)?
                    .page(revision, after.as_deref(), limit);
            }
            Operation::CollectionCommit {
                collection,
                expected_revision,
                metadata,
                edits,
            } => {
                return crate::collection::Collection::open(&self.directory, &path, &collection)?
                    .commit(expected_revision, metadata, edits);
            }
            Operation::CollectionDraftBegin {
                collection,
                draft_id,
                expected_revision,
                content_id,
            } => {
                return crate::draft::begin(
                    &self.directory,
                    &path,
                    &collection,
                    &draft_id,
                    expected_revision,
                    content_id,
                );
            }
            Operation::CollectionDraftAppend {
                collection,
                draft_id,
                sequence,
                edits,
            } => {
                return crate::draft::append(&path, &collection, &draft_id, sequence, edits);
            }
            Operation::CollectionDraftPublish {
                collection,
                draft_id,
                expected_revision,
                metadata,
            } => {
                return crate::draft::publish(
                    &self.directory,
                    &path,
                    &collection,
                    &draft_id,
                    expected_revision,
                    metadata,
                );
            }
            Operation::CollectionDraftAbort {
                collection,
                draft_id,
            } => {
                return crate::draft::abort(&path, &collection, &draft_id);
            }
            _ => (),
        }
        let mut namespace = load_namespace(&path)?;
        match operation {
            Operation::InitializeFeedbackHost { .. } => unreachable!("handled above"),
            Operation::CollectionGet { .. }
            | Operation::CollectionPage { .. }
            | Operation::CollectionCommit { .. }
            | Operation::CollectionDraftBegin { .. }
            | Operation::CollectionDraftAppend { .. }
            | Operation::CollectionDraftPublish { .. }
            | Operation::CollectionDraftAbort { .. } => unreachable!("collections handled above"),
            Operation::Get { key } => Ok(json!({
                "ok": true, "revision": namespace.revision,
                "found": namespace.values.contains_key(&key),
                "value": namespace.values.get(&key),
            })),
            Operation::Snapshot { cursor, limit } => {
                snapshot::page(&namespace.values, namespace.revision, cursor, limit)
            }
            Operation::Set {
                key,
                value,
                expected_revision,
            } => {
                check_revision(&namespace, expected_revision)?;
                namespace.values.insert(key, value);
                persist_mutation(&self.directory, &path, &mut namespace)?;
                Ok(json!({"ok": true, "revision": namespace.revision}))
            }
            Operation::Delete {
                key,
                expected_revision,
            } => {
                check_revision(&namespace, expected_revision)?;
                let deleted = namespace.values.remove(&key).is_some();
                persist_mutation(&self.directory, &path, &mut namespace)?;
                Ok(json!({"ok": true, "revision": namespace.revision, "deleted": deleted}))
            }
        }
    }

    fn namespace_path(&self, caller: &str) -> Result<PathBuf, StateError> {
        if caller.is_empty() || caller.len() > 128 {
            return Err(StateError::new(
                "invalid_caller",
                "caller must contain 1 to 128 UTF-8 bytes",
            ));
        }
        // Hex components cannot contain separators, dots, or platform path prefixes.
        // Chunking also stays below filesystem component limits for long principals.
        let encoded: String = caller
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let mut path = self.directory.join("namespaces");
        validate_directory(&path)?;
        for component in encoded.as_bytes().chunks(64) {
            path.push(std::str::from_utf8(component).expect("hex is ASCII"));
            validate_directory(&path)?;
        }
        Ok(path.join("state.json"))
    }

    fn initialize_feedback_host(
        &self,
        caller: &str,
        claim: amaterasu_module_sdk::state::FeedbackBootstrapClaim,
        resume_existing: bool,
    ) -> Result<Value, StateError> {
        if caller != amaterasu_module_sdk::HOST_LIFECYCLE_CONTROL_PRINCIPAL {
            return Err(StateError::new(
                "permission_denied",
                "only the trusted host may initialize State",
            ));
        }
        if claim.version != 1
            || !valid_hex(&claim.installation_id)
            || !valid_hex(&claim.claim)
            || !amaterasu_module_sdk::valid_name(&claim.pairing_owner)
            || !amaterasu_module_sdk::valid_name(&claim.effect_owner)
        {
            return Err(StateError::new(
                "invalid_request",
                "invalid feedback bootstrap claim",
            ));
        }
        let origin_phase = self.bootstrap_origin_phase(&claim, resume_existing)?;
        let mut history = match self.load_history_record_optional()? {
            Some(history) => history,
            None => match origin_phase {
                Some(LiveFeedbackStateOriginPhase::BootstrapIntent) => {
                    let history = StateHistory {
                        version: 1,
                        phase: StateHistoryPhase::BootstrapIntent,
                        claim: Some(claim.clone()),
                    };
                    self.write_history_record(&history)?;
                    history
                }
                Some(LiveFeedbackStateOriginPhase::Fresh) => StateHistory {
                    version: 1,
                    phase: StateHistoryPhase::Fresh,
                    claim: None,
                },
                _ => {
                    return Err(StateError::new(
                        "bootstrap_history_lost",
                        "State history is missing after prior use or bootstrap",
                    ));
                }
            },
        };
        let namespace_directory = self.directory.join("namespaces");
        let namespace_exists = path_exists(&namespace_directory)?;
        let path = self.namespace_path(&claim.pairing_owner)?;
        let mut namespace = if namespace_exists {
            load_namespace(&path)?
        } else {
            Namespace::default()
        };
        if let Some(record) = namespace
            .values
            .get(amaterasu_module_sdk::state::FEEDBACK_BOOTSTRAP_KEY)
        {
            let phase = record["phase"].as_str().unwrap_or_default();
            if record["claim"] != json!(claim)
                || !matches!(phase, "prepared" | "recovery_started" | "active")
                || history
                    .claim
                    .as_ref()
                    .is_some_and(|stored| stored != &claim)
            {
                return Err(StateError::new(
                    "bootstrap_claim_mismatch",
                    "stored feedback State claim does not match the trusted host",
                ));
            }
            self.commit_bootstrap_history(&claim)?;
            return Ok(json!({
                "ok":true,
                "revision":namespace.revision,
                "claim":claim,
                "phase":phase
            }));
        }

        match history.phase {
            StateHistoryPhase::Fresh => {
                if namespace_exists {
                    return Err(StateError::new(
                        "history_present",
                        "State is not a new store",
                    ));
                }
                match origin_phase {
                    Some(LiveFeedbackStateOriginPhase::BootstrapIntent) => {
                        self.write_history_record(&StateHistory {
                            version: 1,
                            phase: StateHistoryPhase::BootstrapIntent,
                            claim: Some(claim.clone()),
                        })?;
                    }
                    Some(LiveFeedbackStateOriginPhase::BootstrapCommitted) => {
                        return Err(StateError::new(
                            "bootstrap_history_lost",
                            "committed feedback State history is missing",
                        ));
                    }
                    Some(LiveFeedbackStateOriginPhase::Used) => {
                        return Err(StateError::new(
                            "history_present",
                            "State is not a new store",
                        ));
                    }
                    Some(LiveFeedbackStateOriginPhase::Fresh) | None => {
                        self.update_state_origin(
                            LiveFeedbackStateOriginPhase::BootstrapIntent,
                            Some(&claim.claim),
                        )?;
                        history.phase = StateHistoryPhase::BootstrapIntent;
                        history.claim = Some(claim.clone());
                        self.write_history_record(&history)?;
                    }
                }
            }
            StateHistoryPhase::BootstrapIntent
                if history.claim.as_ref() == Some(&claim)
                    && namespace.revision == 0
                    && namespace.values.is_empty() => {}
            StateHistoryPhase::BootstrapIntent => {
                return Err(StateError::new(
                    "bootstrap_claim_mismatch",
                    "another feedback State claim is already pending",
                ));
            }
            StateHistoryPhase::BootstrapCommitted if history.claim.as_ref() == Some(&claim) => {
                return Err(StateError::new(
                    "bootstrap_history_lost",
                    "committed feedback State history is missing",
                ));
            }
            StateHistoryPhase::BootstrapCommitted => {
                return Err(StateError::new(
                    "bootstrap_claim_mismatch",
                    "committed feedback State claim differs from the trusted host",
                ));
            }
            StateHistoryPhase::Used if resume_existing => {
                return Err(StateError::new(
                    "bootstrap_history_lost",
                    "State has prior history but the feedback claim is missing",
                ));
            }
            StateHistoryPhase::Used => {
                return Err(StateError::new(
                    "history_present",
                    "State is not a new store",
                ));
            }
        }

        if namespace.revision != 0 || !namespace.values.is_empty() {
            return Err(StateError::new(
                "history_present",
                "feedback claim cannot replace existing State namespace data",
            ));
        }
        namespace.values.insert(
            amaterasu_module_sdk::state::FEEDBACK_BOOTSTRAP_KEY.into(),
            json!({"claim":claim,"phase":"prepared"}),
        );
        persist_mutation(&self.directory, &path, &mut namespace)?;
        self.commit_bootstrap_history(&claim)?;
        Ok(json!({
            "ok":true,
            "revision":namespace.revision,
            "claim":claim,
            "phase":"prepared"
        }))
    }

    fn ensure_history_record(&self) -> Result<(), StateError> {
        match std::fs::symlink_metadata(self.history_path()) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                let history = self.load_history_record()?;
                if history.phase == StateHistoryPhase::Fresh
                    && self
                        .load_state_origin()?
                        .is_some_and(|origin| origin.phase == LiveFeedbackStateOriginPhase::Used)
                {
                    self.write_history_record(&StateHistory {
                        version: 1,
                        phase: StateHistoryPhase::Used,
                        claim: None,
                    })?;
                }
                Ok(())
            }
            Ok(_) => Err(StateError::new(
                "corrupt_storage",
                "State history marker is not a regular file",
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let namespaces_exist = path_exists(&self.directory.join("namespaces"))?;
                match self.load_state_origin()? {
                    Some(mut origin) => match origin.phase {
                        LiveFeedbackStateOriginPhase::Fresh => {
                            let phase = if namespaces_exist {
                                origin.phase = LiveFeedbackStateOriginPhase::Used;
                                LiveFeedbackStateOriginPhase::Used
                            } else {
                                LiveFeedbackStateOriginPhase::Fresh
                            };
                            self.write_state_origin(&origin)?;
                            self.write_history_record(&StateHistory {
                                version: 1,
                                phase: match phase {
                                    LiveFeedbackStateOriginPhase::Fresh => StateHistoryPhase::Fresh,
                                    LiveFeedbackStateOriginPhase::Used => StateHistoryPhase::Used,
                                    _ => unreachable!("fresh origin maps to a basic history phase"),
                                },
                                claim: None,
                            })
                        }
                        LiveFeedbackStateOriginPhase::BootstrapIntent => Ok(()),
                        LiveFeedbackStateOriginPhase::Used
                        | LiveFeedbackStateOriginPhase::BootstrapCommitted => Ok(()),
                    },
                    None => {
                        let phase = if namespaces_exist {
                            StateHistoryPhase::Used
                        } else {
                            StateHistoryPhase::Fresh
                        };
                        self.write_history_record(&StateHistory {
                            version: 1,
                            phase,
                            claim: None,
                        })
                    }
                }
            }
            Err(error) => Err(storage_error(error)),
        }
    }

    fn load_history_record(&self) -> Result<StateHistory, StateError> {
        self.load_history_record_optional()?.ok_or_else(|| {
            StateError::new("bootstrap_history_lost", "State history marker is missing")
        })
    }

    fn load_history_record_optional(&self) -> Result<Option<StateHistory>, StateError> {
        let path = self.history_path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage_error(error)),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(StateError::new(
                "corrupt_storage",
                "State history marker is not a regular file",
            ));
        }
        let bytes = read_bounded(&path, 4096).map_err(storage_error)?;
        let history: StateHistory = serde_json::from_slice(&bytes)
            .map_err(|error| StateError::new("corrupt_storage", error.to_string()))?;
        let claim_required = matches!(
            history.phase,
            StateHistoryPhase::BootstrapIntent | StateHistoryPhase::BootstrapCommitted
        );
        if history.version != 1 || history.claim.is_some() != claim_required {
            return Err(StateError::new(
                "corrupt_storage",
                "State history marker has an invalid version or claim",
            ));
        }
        Ok(Some(history))
    }

    fn write_history_record(&self, history: &StateHistory) -> Result<(), StateError> {
        write_json_atomically(&self.history_path(), history).map_err(storage_error)
    }

    fn record_state_use(&self) -> Result<(), StateError> {
        let mut history = self.load_history_record()?;
        match history.phase {
            StateHistoryPhase::Fresh => {
                if let Some(origin) = self.load_state_origin()? {
                    match origin.phase {
                        LiveFeedbackStateOriginPhase::Fresh => {
                            self.update_state_origin(LiveFeedbackStateOriginPhase::Used, None)?
                        }
                        LiveFeedbackStateOriginPhase::Used => (),
                        LiveFeedbackStateOriginPhase::BootstrapIntent
                        | LiveFeedbackStateOriginPhase::BootstrapCommitted => {
                            return Err(StateError::new(
                                "bootstrap_pending",
                                "State is completing a feedback bootstrap claim",
                            ));
                        }
                    }
                }
                history.phase = StateHistoryPhase::Used;
                self.write_history_record(&history)
            }
            StateHistoryPhase::BootstrapIntent => Err(StateError::new(
                "bootstrap_pending",
                "State is completing a feedback bootstrap claim",
            )),
            StateHistoryPhase::Used | StateHistoryPhase::BootstrapCommitted => Ok(()),
        }
    }

    fn commit_bootstrap_history(
        &self,
        claim: &amaterasu_module_sdk::state::FeedbackBootstrapClaim,
    ) -> Result<(), StateError> {
        let mut history = self.load_history_record()?;
        if history.claim.as_ref().is_some_and(|stored| stored != claim) {
            return Err(StateError::new(
                "bootstrap_claim_mismatch",
                "State history marker belongs to another feedback claim",
            ));
        }
        self.update_state_origin(
            LiveFeedbackStateOriginPhase::BootstrapCommitted,
            Some(&claim.claim),
        )?;
        history.phase = StateHistoryPhase::BootstrapCommitted;
        history.claim = Some(claim.clone());
        self.write_history_record(&history)
    }

    fn bootstrap_origin_phase(
        &self,
        claim: &amaterasu_module_sdk::state::FeedbackBootstrapClaim,
        resume_existing: bool,
    ) -> Result<Option<LiveFeedbackStateOriginPhase>, StateError> {
        let Some(origin) = self.load_state_origin()? else {
            return Ok(None);
        };
        if origin.installation_id != claim.installation_id {
            return Err(StateError::new(
                "bootstrap_claim_mismatch",
                "feedback State origin belongs to another installation",
            ));
        }
        match origin.phase {
            LiveFeedbackStateOriginPhase::Fresh => {
                if origin.bootstrap_claim.is_some() {
                    return Err(StateError::new(
                        "corrupt_storage",
                        "fresh State origin unexpectedly has a bootstrap claim",
                    ));
                }
            }
            LiveFeedbackStateOriginPhase::Used => {
                return Err(if resume_existing {
                    StateError::new(
                        "bootstrap_history_lost",
                        "State has prior history but the feedback claim is missing",
                    )
                } else {
                    StateError::new("history_present", "State is not a new store")
                });
            }
            LiveFeedbackStateOriginPhase::BootstrapIntent
            | LiveFeedbackStateOriginPhase::BootstrapCommitted => {
                if origin.bootstrap_claim.as_deref() != Some(claim.claim.as_str()) {
                    return Err(StateError::new(
                        "bootstrap_claim_mismatch",
                        "State origin belongs to another feedback bootstrap claim",
                    ));
                }
            }
        }
        Ok(Some(origin.phase))
    }

    fn load_state_origin(&self) -> Result<Option<LiveFeedbackStateOrigin>, StateError> {
        let path = self.directory.join(STATE_ORIGIN_FILE);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage_error(error)),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(StateError::new(
                "corrupt_storage",
                "State origin marker is not a regular file",
            ));
        }
        let bytes = read_bounded(&path, 4096).map_err(storage_error)?;
        let origin: LiveFeedbackStateOrigin = serde_json::from_slice(&bytes)
            .map_err(|error| StateError::new("corrupt_storage", error.to_string()))?;
        let claim_valid = match (origin.phase, origin.bootstrap_claim.as_deref()) {
            (LiveFeedbackStateOriginPhase::Fresh | LiveFeedbackStateOriginPhase::Used, None) => {
                true
            }
            (
                LiveFeedbackStateOriginPhase::BootstrapIntent
                | LiveFeedbackStateOriginPhase::BootstrapCommitted,
                Some(claim),
            ) => valid_hex(claim),
            _ => false,
        };
        if origin.version != 1
            || !valid_hex(&origin.installation_id)
            || !valid_hex(&origin.state_root_identity)
            || !claim_valid
        {
            return Err(StateError::new(
                "corrupt_storage",
                "State origin marker has an invalid version, identity, phase, or claim",
            ));
        }
        Ok(Some(origin))
    }

    fn write_state_origin(&self, origin: &LiveFeedbackStateOrigin) -> Result<(), StateError> {
        write_json_atomically(&self.directory.join(STATE_ORIGIN_FILE), origin)
            .map_err(storage_error)
    }

    fn update_state_origin(
        &self,
        phase: LiveFeedbackStateOriginPhase,
        bootstrap_claim: Option<&str>,
    ) -> Result<(), StateError> {
        let Some(mut origin) = self.load_state_origin()? else {
            return Ok(());
        };
        let token_matches = origin.bootstrap_claim.as_deref() == bootstrap_claim;
        let allowed = match (origin.phase, phase) {
            (LiveFeedbackStateOriginPhase::Fresh, LiveFeedbackStateOriginPhase::Used) => {
                bootstrap_claim.is_none()
            }
            (
                LiveFeedbackStateOriginPhase::Fresh,
                LiveFeedbackStateOriginPhase::BootstrapIntent,
            ) => bootstrap_claim.is_some_and(valid_hex),
            (
                LiveFeedbackStateOriginPhase::BootstrapIntent,
                LiveFeedbackStateOriginPhase::BootstrapIntent
                | LiveFeedbackStateOriginPhase::BootstrapCommitted,
            ) => token_matches,
            (
                LiveFeedbackStateOriginPhase::BootstrapCommitted,
                LiveFeedbackStateOriginPhase::BootstrapCommitted,
            ) => token_matches,
            (LiveFeedbackStateOriginPhase::Used, LiveFeedbackStateOriginPhase::Used) => {
                bootstrap_claim.is_none()
            }
            _ => false,
        };
        if !allowed {
            return Err(StateError::new(
                "bootstrap_claim_mismatch",
                "State origin phase cannot advance for this claim",
            ));
        }
        origin.phase = phase;
        origin.bootstrap_claim = bootstrap_claim.map(str::to_owned);
        self.write_state_origin(&origin)
    }

    fn history_path(&self) -> PathBuf {
        self.directory.join(HISTORY_FILE)
    }
}

fn path_exists(path: &Path) -> Result<bool, StateError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage_error(error)),
    }
}

fn valid_hex(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_operation(operation: &Operation) -> Result<(), StateError> {
    match operation {
        Operation::InitializeFeedbackHost { .. } => Ok(()),
        Operation::CollectionGet { collection, .. }
        | Operation::CollectionPage { collection, .. }
        | Operation::CollectionCommit { collection, .. }
        | Operation::CollectionDraftBegin { collection, .. }
        | Operation::CollectionDraftAppend { collection, .. }
        | Operation::CollectionDraftPublish { collection, .. }
        | Operation::CollectionDraftAbort { collection, .. } => validate_key(collection),
        Operation::Get { key } | Operation::Delete { key, .. } => validate_key(key),
        Operation::Set { key, value, .. } => {
            validate_key(key)?;
            validate_value(value)?;
            snapshot::validate_entry(key, value)
        }
        Operation::Snapshot { cursor, limit } => {
            if limit.is_some_and(|limit| limit == 0 || limit > snapshot::MAX_PAGE_KEYS) {
                return Err(StateError::new(
                    "invalid_request",
                    "snapshot limit must be between 1 and 128",
                ));
            }
            if let Some(cursor) = cursor {
                validate_key(&cursor.after)?;
            }
            Ok(())
        }
    }
}

pub(crate) fn validate_key(key: &str) -> Result<(), StateError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(StateError::new(
            "invalid_key",
            "key must contain 1 to 256 UTF-8 bytes without control characters",
        ));
    }
    Ok(())
}

fn validate_value(value: &Value) -> Result<(), StateError> {
    let bytes = serde_json::to_vec(value).map_err(storage_error)?;
    if bytes.len() > MAX_VALUE_BYTES {
        return Err(StateError::new(
            "value_too_large",
            "encoded value exceeds 16384 bytes",
        ));
    }
    Ok(())
}

fn load_namespace(path: &Path) -> Result<Namespace, StateError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Namespace::default());
        }
        Err(error) => return Err(storage_error(error)),
        Ok(_) => (),
    }
    let bytes = read_bounded(path, MAX_NAMESPACE_BYTES).map_err(storage_error)?;
    let namespace: Namespace = serde_json::from_slice(&bytes)
        .map_err(|error| StateError::new("corrupt_storage", error.to_string()))?;
    if namespace.format_version != 1 {
        return Err(StateError::new(
            "corrupt_storage",
            "unsupported state format version",
        ));
    }
    validate_namespace(&namespace).map_err(|_| {
        StateError::new(
            "corrupt_storage",
            "stored namespace violates size or key limits",
        )
    })?;
    Ok(namespace)
}

fn validate_namespace(namespace: &Namespace) -> Result<(), StateError> {
    if namespace.values.len() > MAX_KEYS {
        return Err(StateError::new(
            "namespace_full",
            "namespace exceeds 4096 keys",
        ));
    }
    for (key, value) in &namespace.values {
        validate_key(key)?;
        validate_value(value)?;
    }
    if serde_json::to_vec_pretty(namespace)
        .map_err(storage_error)?
        .len() as u64
        > MAX_NAMESPACE_BYTES
    {
        return Err(StateError::new(
            "namespace_full",
            "encoded namespace exceeds 1048576 bytes",
        ));
    }
    Ok(())
}

fn check_revision(namespace: &Namespace, expected: Option<u64>) -> Result<(), StateError> {
    if expected.is_some_and(|revision| revision != namespace.revision) {
        return Err(StateError::conflict(namespace.revision));
    }
    Ok(())
}

fn persist_mutation(root: &Path, path: &Path, namespace: &mut Namespace) -> Result<(), StateError> {
    namespace.revision = namespace.revision.checked_add(1).ok_or_else(|| {
        StateError::new("revision_exhausted", "namespace revision cannot advance")
    })?;
    validate_namespace(namespace)?;
    let directory = path.parent().expect("namespace files have a parent");
    create_namespace_directory(root, directory)?;
    write_json_atomically(path, namespace).map_err(storage_error)
}

pub(crate) fn validate_directory(path: &Path) -> Result<(), StateError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_error(error)),
        Ok(_) => Err(StateError::new(
            "storage_error",
            "namespace directory is not a real directory",
        )),
    }
}

pub(crate) fn create_namespace_directory(root: &Path, directory: &Path) -> Result<(), StateError> {
    let relative = directory.strip_prefix(root).map_err(storage_error)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let parent = current.clone();
        current.push(component);
        create_private_directory(&current).map_err(storage_error)?;
        // Persist each directory entry before acknowledging its first state mutation.
        sync_directory(&parent).map_err(storage_error)?;
    }
    Ok(())
}

pub(crate) fn storage_error(error: impl std::fmt::Display) -> StateError {
    StateError::new("storage_error", error.to_string())
}

#[cfg(test)]
mod tests;
