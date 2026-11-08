//! Caller-scoped write-ahead journal for interrupted stream-session cleanup.
//!
//! The state module owns the private runtime state directory. This journal stores
//! only stable cleanup identity, monitor specifications and attempted-resource flags; never stream keys,
//! peer addresses, negotiated configuration, resource handles, or lease tokens.
use super::{record::Record, rpc};
use crate::model::PrepareConfig;
use amaterasu_contracts::virtual_display::{
    VirtualDisplayIntent, VirtualDisplayLayout, VirtualDisplayMode,
};
use amaterasu_module_sdk::{ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const KEY: &str = "stream-session/lifecycle/v1";
const FORMAT_VERSION: u32 = 1;
const MAX_ENTRIES: usize = 32;

fn is_false(value: &bool) -> bool {
    !*value
}

pub(super) type Send<'a> = dyn FnMut(amaterasu_module_sdk::Command) -> ModuleResult<Response> + 'a;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    #[serde(
        default,
        skip_serializing_if = "crate::providers::ProviderServices::is_default"
    )]
    pub providers: crate::providers::ProviderServices,
    pub record_id: u64,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_recovery: Option<LifecycleRecovery>,
    pub attempted_resources: BTreeSet<String>,
    pub input_attempted: bool,
    #[serde(default)]
    pub virtual_display_attempted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub virtual_display: Option<VirtualDisplayRecovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_capture: Option<super::shared_capture::Source>,
    pub display_attempted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_operation_id: Option<String>,
    // Read-only v1 migration markers. An old effect is never silently retired.
    #[serde(
        default,
        rename = "limiter_attempted",
        skip_serializing_if = "is_false"
    )]
    pub legacy_effect_pending: bool,
    #[serde(
        default,
        rename = "limiter_executable_path",
        skip_serializing_if = "Option::is_none"
    )]
    pub legacy_effect_target: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct LifecycleRecovery {
    version: u32,
    session_target: amaterasu_extension_api::LifecycleTarget,
    stream_target: Option<amaterasu_extension_api::LifecycleTarget>,
    plan_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preparation: Option<PreparationRecovery>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct PreparationRecovery {
    pub operation_id: String,
    pub caller: String,
}

impl PreparationRecovery {
    pub fn lookup(
        &self,
        session: &amaterasu_extension_api::LifecycleTarget,
    ) -> amaterasu_extension_api::StreamPreparationLookup {
        amaterasu_extension_api::StreamPreparationLookup {
            operation:
                amaterasu_extension_api::StreamPreparationOperation::ResolveStreamPreparation,
            session: session.clone(),
            preparation_operation_id: self.operation_id.clone(),
            preparation_caller: self.caller.clone(),
        }
    }
}

/// Only the provider's bounded cleanup identity and specification survive restart.
/// Mode policy, topology, device paths and leases are deliberately not persisted.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct VirtualDisplayRecovery {
    identity: String,
    reference_id: String,
    name: String,
    width: u32,
    height: u32,
    refresh_millihz: u32,
}

impl VirtualDisplayRecovery {
    fn from_intent(intent: &VirtualDisplayIntent) -> Self {
        Self {
            identity: intent.identity.clone(),
            reference_id: intent.reference_id.clone(),
            name: intent.name.clone(),
            width: intent.width,
            height: intent.height,
            refresh_millihz: intent.refresh_millihz,
        }
    }

    fn recovery_intent(&self) -> VirtualDisplayIntent {
        VirtualDisplayIntent {
            identity: self.identity.clone(),
            reference_id: self.reference_id.clone(),
            name: self.name.clone(),
            width: self.width,
            height: self.height,
            refresh_millihz: self.refresh_millihz,
            dd_virtual_display_scale: 0,
            dd_activate_virtual_display: true,
            dd_use_sunshine_virtual_display_driver: false,
            virtual_display_mode: VirtualDisplayMode::PerClient,
            virtual_display_layout: VirtualDisplayLayout::Extended,
            request_virtual_display: Some(true),
            display_policy: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    pub format_version: u32,
    pub entries: BTreeMap<u64, Entry>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

impl Entry {
    pub fn from_record(record: &Record) -> Self {
        Self {
            providers: record.providers.clone(),
            record_id: record.id,
            owner: record.owner.clone(),
            lifecycle_recovery: record.config.lifecycle_session_target.clone().map(
                |session_target| LifecycleRecovery {
                    version: 1,
                    session_target,
                    stream_target: record
                        .start
                        .as_ref()
                        .and_then(|start| start.lifecycle_target.clone())
                        .or_else(|| record.restart_lifecycle_target.clone()),
                    plan_revision: record.start.as_ref().and_then(|start| {
                        start
                            .lifecycle_candidate
                            .as_ref()
                            .map(|candidate| candidate.plan_revision)
                    }),
                    preparation: record.restart_lifecycle_preparation.clone().or_else(|| {
                        (record.owner == "amaterasu.rtsp"
                            && record.config.lifecycle_capture_mode.is_external())
                        .then(|| PreparationRecovery {
                            operation_id: format!(
                                "{}-lifecycle-prepare",
                                record.config.operation_id
                            ),
                            caller: record.owner.clone(),
                        })
                    }),
                },
            ),
            attempted_resources: record
                .attempted
                .iter()
                .map(|resource| resource.name().to_owned())
                .collect(),
            input_attempted: record.input_attempted,
            virtual_display_attempted: record.virtual_display_attempted,
            virtual_display: record
                .virtual_display_attempted
                .then(|| {
                    record
                        .config
                        .virtual_display_intent
                        .as_ref()
                        .map(VirtualDisplayRecovery::from_intent)
                })
                .flatten(),
            shared_capture: record
                .shared_capture_attached
                .then(|| record.shared_capture.clone())
                .flatten(),
            display_attempted: record.display_attempted,
            display_operation_id: record.display_attempted.then(|| record.display_operation()),
            legacy_effect_pending: false,
            legacy_effect_target: None,
        }
    }

    pub fn validate(&self) -> bool {
        self.providers.validate().is_ok()
            && self.record_id != 0
            && !self.owner.is_empty()
            && self.owner.len() <= 128
            && !self.owner.chars().any(char::is_control)
            && self.virtual_display.as_ref().is_none_or(|display| {
                self.virtual_display_attempted && display.recovery_intent().validate().is_ok()
            })
            && self
                .shared_capture
                .as_ref()
                .is_none_or(|source| self.virtual_display_attempted && source.valid())
            && self.display_operation_id.as_ref().is_none_or(|operation| {
                self.display_attempted
                    && !operation.is_empty()
                    && operation.len() <= 128
                    && operation.bytes().all(|byte| byte.is_ascii_graphic())
            })
            && !self.legacy_effect_pending
            && self.legacy_effect_target.is_none()
            && self.attempted_resources.len() <= 6
            && self.attempted_resources.iter().all(|resource| {
                matches!(
                    resource.as_str(),
                    "session"
                        | "audio-encode"
                        | "media-transport"
                        | "control-transport"
                        | "video"
                        | "audio"
                )
            })
    }

    pub fn recovery_record(&self) -> Result<Record, &'static str> {
        if !self.validate() {
            return Err("invalid_lifecycle_journal_entry");
        }
        let config: PrepareConfig = serde_json::from_value(json!({
            "peer_ip":"127.0.0.1",
            "key_hex":"00000000000000000000000000000000",
            "key_id":0,
            "connect_data":0,
            "operation_id":format!("restart-recovery-{:x}", self.record_id)
        }))
        .map_err(|_| "invalid_lifecycle_journal_entry")?;
        let mut record = Record::new(self.record_id, self.owner.clone(), config, 30_000);
        if let Some(recovery) = &self.lifecycle_recovery {
            if recovery.version != 1
                || recovery.plan_revision.is_some()
                || (recovery.stream_target.is_none()
                    && recovery.preparation.as_ref().is_none_or(|preparation| {
                        preparation.caller != "amaterasu.rtsp"
                            || preparation.caller != self.owner
                            || amaterasu_extension_api::validate_stream_preparation_lookup(
                                &preparation.lookup(&recovery.session_target),
                            )
                            .is_err()
                    }))
            {
                return Err("lifecycle_reconciliation_required");
            }
            record.restart_lifecycle_target = recovery.stream_target.clone();
            record.config.lifecycle_session_target = Some(recovery.session_target.clone());
            record.restart_lifecycle_preparation = recovery
                .stream_target
                .is_none()
                .then(|| recovery.preparation.clone())
                .flatten();
            let mut nonce = [0u8; 16];
            getrandom::fill(&mut nonce).map_err(|_| "entropy_unavailable")?;
            record.restart_lifecycle_operation = Some(format!(
                "ss-{:016x}-restart-retire-{:032x}",
                self.record_id,
                u128::from_le_bytes(nonce)
            ));
            // This is a retired-process cleanup marker, not a live binding fence.
            // The coordinator verifies the old epoch and exact consumer scope.
            record.lifecycle_fenced = true;
        }
        record.providers = self.providers.clone();
        record.attempted = self
            .attempted_resources
            .iter()
            .map(|name| resource_from_name(name).ok_or("invalid_lifecycle_journal_entry"))
            .collect::<Result<_, _>>()?;
        record.input_attempted = self.input_attempted;
        record.input_lookup_required = self.input_attempted;
        record.virtual_display_attempted = self.virtual_display_attempted;
        record.shared_capture = self.shared_capture.clone();
        record.shared_capture_attached = self.shared_capture.is_some();
        record.config.virtual_display_intent = self
            .virtual_display
            .as_ref()
            .map(VirtualDisplayRecovery::recovery_intent);
        record.display_operation_id = self.display_operation_id.clone();
        record.display_attempted = self.display_attempted;
        record.state = super::record::State::Stopping;
        record.outcome = super::record::State::Failed;
        record.error = Some("host_restart_recovery");
        Ok(record)
    }
}

fn resource_from_name(name: &str) -> Option<super::record::Resource> {
    use super::record::Resource;
    match name {
        "session" => Some(Resource::Session),
        "audio-encode" => Some(Resource::AudioEncoder),
        "media-transport" => Some(Resource::Media),
        "control-transport" => Some(Resource::Control),
        "video" => Some(Resource::Video),
        "audio" => Some(Resource::AudioCapture),
        _ => None,
    }
}

pub(super) struct LifecycleJournal {
    nonce: String,
    pub loaded: bool,
    pub failed: bool,
    pub revision: u64,
    pub document: Document,
    load: Option<rpc::Pending>,
    load_validated: bool,
    load_next_poll: u64,
}

pub(super) enum LoadProgress {
    Waiting,
    Ready,
    Failed,
}

impl LifecycleJournal {
    pub fn new() -> Result<Self, &'static str> {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|_| "randomness_unavailable")?;
        Ok(Self {
            nonce: nonce.iter().map(|byte| format!("{byte:02x}")).collect(),
            loaded: false,
            failed: false,
            revision: 0,
            document: Document::default(),
            load: None,
            load_validated: false,
            load_next_poll: 0,
        })
    }

    #[cfg(test)]
    pub fn empty_for_test() -> Self {
        Self {
            nonce: "test".into(),
            loaded: true,
            failed: false,
            revision: 0,
            document: Document::default(),
            load: None,
            load_validated: false,
            load_next_poll: 0,
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.document.entries.values()
    }

    pub fn load_request_id(&self) -> Option<u64> {
        self.load.as_ref().and_then(rpc::Pending::request_id)
    }

    pub fn load_was_sent(&self) -> bool {
        self.load.as_ref().is_some_and(rpc::Pending::was_sent)
    }

    pub fn matches_record(&self, record: &Record) -> bool {
        self.document
            .entries
            .get(&record.id)
            .is_some_and(|entry| *entry == Entry::from_record(record))
    }

    pub fn matches_lifecycle_identity(&self, record: &Record) -> bool {
        self.document.entries.get(&record.id).is_some_and(|entry| {
            entry.lifecycle_recovery == Entry::from_record(record).lifecycle_recovery
        })
    }

    pub fn with_record(&self, record: &Record) -> Result<Document, &'static str> {
        let mut next = self.document.clone();
        next.entries.insert(record.id, Entry::from_record(record));
        validate_document(&next)?;
        Ok(next)
    }

    pub fn without_record(&self, record_id: u64) -> Document {
        let mut next = self.document.clone();
        next.entries.remove(&record_id);
        next
    }

    pub fn set_payload(&self, document: &Document) -> Value {
        json!({
            "action":"set",
            "key":KEY,
            "value":document,
            "expected_revision":self.revision
        })
    }

    pub fn commit(&mut self, document: Document, response: &Value) -> Result<(), &'static str> {
        validate_document(&document)?;
        let revision = response["revision"]
            .as_u64()
            .filter(|revision| *revision == self.revision.saturating_add(1))
            .ok_or("lifecycle_journal_revision_unconfirmed")?;
        self.revision = revision;
        self.document = document;
        Ok(())
    }

    pub fn progress_load(&mut self, send: &mut Send<'_>, now: u64) -> LoadProgress {
        if self.failed {
            return LoadProgress::Failed;
        }
        if self.loaded {
            return LoadProgress::Ready;
        }
        if now < self.load_next_poll {
            return LoadProgress::Waiting;
        }
        if self.load.is_none() {
            self.load = Some(
                rpc::Pending::new(
                    "amaterasu.state/1",
                    json!({"action":"get","key":KEY}),
                    format!("ss-journal-{}-load", self.nonce),
                    None,
                )
                .retry_uncertain_admission(),
            );
        }
        let progress = self
            .load
            .as_mut()
            .expect("load was created")
            .poll(send, now);
        match progress {
            rpc::Progress::Waiting => LoadProgress::Waiting,
            rpc::Progress::Unresolved => {
                self.load.as_mut().expect("load remains owned").retry();
                self.load_next_poll = now.saturating_add(1000);
                LoadProgress::Waiting
            }
            rpc::Progress::Failed => {
                self.failed = true;
                if self
                    .load
                    .as_ref()
                    .is_some_and(rpc::Pending::admission_rejected)
                {
                    self.load = None;
                }
                LoadProgress::Failed
            }
            rpc::Progress::Result(payload) => {
                if !self.load_validated {
                    if self.accept_load(&payload).is_err() {
                        self.failed = true;
                        return LoadProgress::Failed;
                    }
                    self.load_validated = true;
                }
                let released = self
                    .load
                    .as_mut()
                    .expect("load result remains owned")
                    .release_result(send);
                if !released {
                    self.load_next_poll = now.saturating_add(1000);
                    return LoadProgress::Waiting;
                }
                self.load = None;
                self.loaded = true;
                LoadProgress::Ready
            }
        }
    }

    fn accept_load(&mut self, payload: &Value) -> Result<(), &'static str> {
        if payload["ok"] != true {
            return Err("lifecycle_journal_read_failed");
        }
        self.revision = payload["revision"]
            .as_u64()
            .ok_or("lifecycle_journal_revision_unconfirmed")?;
        if payload["found"] == true {
            let document: Document = serde_json::from_value(payload["value"].clone())
                .map_err(|_| "invalid_lifecycle_journal")?;
            validate_document(&document)?;
            self.document = document;
        } else if payload["found"] == false {
            self.document = Document::default();
        } else {
            return Err("invalid_lifecycle_journal");
        }
        Ok(())
    }

    pub fn validate_saved_response(
        &mut self,
        payload: &Value,
        document: Document,
    ) -> Result<(), &'static str> {
        if payload["ok"] != true {
            return Err("lifecycle_journal_write_failed");
        }
        self.commit(document, payload)
    }

    pub fn op_id(&self, sequence: u64) -> String {
        format!("ss-journal-{}-{sequence}", self.nonce)
    }
}

fn validate_document(document: &Document) -> Result<(), &'static str> {
    if document.format_version != FORMAT_VERSION
        || document.entries.len() > MAX_ENTRIES
        || document
            .entries
            .iter()
            .any(|(id, entry)| *id != entry.record_id || !entry.validate())
    {
        return Err("invalid_lifecycle_journal");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PrepareConfig;
    use serde_json::json;

    #[test]
    fn startup_load_keeps_validated_result_when_release_acknowledgement_is_lost() {
        let mut journal = LifecycleJournal::new().unwrap();
        let mut send = |command| match command {
            amaterasu_module_sdk::Command::Request { .. } => Ok(Response::Accepted { id: 9 }),
            amaterasu_module_sdk::Command::TakeResult { request: 9 } => Ok(Response::Result {
                payload: json!({"ok":true,"found":false,"revision":7}),
            }),
            amaterasu_module_sdk::Command::ReleaseResult { request: 9 } => {
                Err("lost release".into())
            }
            _ => panic!("unexpected journal command"),
        };
        assert!(matches!(
            journal.progress_load(&mut send, 1),
            LoadProgress::Waiting
        ));
        assert_eq!(journal.load_request_id(), Some(9));
        assert!(matches!(
            journal.progress_load(&mut send, 2),
            LoadProgress::Waiting
        ));
        assert!(!journal.failed);
        assert_eq!(journal.revision, 7);
        assert!(matches!(
            journal.progress_load(
                &mut |command| match command {
                    amaterasu_module_sdk::Command::ReleaseResult { request: 9 } =>
                        Ok(Response::error("not_found", "already released")),
                    _ => panic!("must retry only release"),
                },
                1002
            ),
            LoadProgress::Ready
        ));
        assert!(journal.loaded);
        assert_eq!(journal.revision, 7);
    }

    #[test]
    fn startup_load_retries_transient_result_failures_without_cancelling() {
        let mut journal = LifecycleJournal::new().unwrap();
        assert!(matches!(
            journal.progress_load(
                &mut |command| match command {
                    amaterasu_module_sdk::Command::Request { .. } =>
                        Ok(Response::Accepted { id: 9 }),
                    _ => panic!("expected read admission"),
                },
                1
            ),
            LoadProgress::Waiting
        ));
        for now in 2..=4 {
            assert!(matches!(
                journal.progress_load(
                    &mut |command| match command {
                        amaterasu_module_sdk::Command::TakeResult { request: 9 } =>
                            Err("temporary failure".into()),
                        _ => panic!("must not cancel or replay the read"),
                    },
                    now
                ),
                LoadProgress::Waiting
            ));
        }
        assert!(matches!(
            journal.progress_load(&mut |_| panic!("backoff must not poll"), 5),
            LoadProgress::Waiting
        ));
        assert!(matches!(
            journal.progress_load(&mut |_| panic!("backoff must not poll"), 6),
            LoadProgress::Waiting
        ));
        assert!(matches!(
            journal.progress_load(
                &mut |command| match command {
                    amaterasu_module_sdk::Command::TakeResult { request: 9 } =>
                        Ok(Response::Result {
                            payload: json!({"ok":true,"found":false,"revision":0})
                        }),
                    amaterasu_module_sdk::Command::ReleaseResult { request: 9 } => Ok(Response::Ok),
                    _ => panic!("must retain the same request"),
                },
                1005
            ),
            LoadProgress::Ready
        ));
        assert!(!journal.failed);
    }

    #[test]
    fn failed_load_keeps_an_uncertain_admission_owned_for_shutdown() {
        let mut journal = LifecycleJournal::new().unwrap();
        assert!(matches!(
            journal.progress_load(&mut |_| Err("lost admission".into()), 1),
            LoadProgress::Waiting
        ));
        assert!(matches!(
            journal.progress_load(
                &mut |_| Ok(Response::error("unavailable", "retry rejected")),
                2
            ),
            LoadProgress::Failed
        ));
        assert!(journal.failed);
        assert!(journal.load_was_sent());
    }

    #[test]
    fn lifecycle_marker_survives_restart_and_blocks_legacy_recovery() {
        let mut record = record();
        record.config.lifecycle_session_target = Some(serde_json::from_value(json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1})).unwrap());
        let entry = Entry::from_record(&record);
        let encoded = serde_json::to_value(&entry).unwrap();
        assert_eq!(encoded["lifecycle_recovery"]["version"], 1);
        assert_eq!(
            encoded["lifecycle_recovery"]["session_target"]["session_id"],
            "session"
        );
        let restored: Entry = serde_json::from_value(encoded).unwrap();
        assert!(matches!(
            restored.recovery_record(),
            Err("lifecycle_reconciliation_required")
        ));
    }

    fn record() -> Record {
        let config: PrepareConfig = serde_json::from_value(json!({
            "peer_ip":"127.0.0.1",
            "key_hex":"0123456789abcdef0123456789abcdef",
            "key_id":1,
            "connect_data":2,
            "operation_id":"prepare-one"
        }))
        .unwrap();
        let mut record = Record::new(0x12ab, "client.principal".into(), config, 30_000);
        record
            .attempted
            .insert(super::super::record::Resource::Media);
        record
            .attempted
            .insert(super::super::record::Resource::Video);
        record.input_attempted = true;
        record.display_attempted = true;
        record
    }

    #[test]
    fn journal_round_trip_reconstructs_exact_cleanup_identity_without_secrets() {
        let record = record();
        let entry = Entry::from_record(&record);
        assert!(entry.validate());
        assert_eq!(entry.record_id, record.id);
        assert_eq!(entry.owner, record.owner);

        let recovered = entry.recovery_record().unwrap();
        assert_eq!(recovered.id, record.id);
        assert_eq!(recovered.owner, record.owner);
        assert_eq!(
            recovered.operation(super::super::record::Resource::Media),
            record.operation(super::super::record::Resource::Media)
        );
        assert_eq!(recovered.input_operation(), record.input_operation());
        assert!(recovered.input_attempted);
        assert!(recovered.display_attempted);
        assert_eq!(recovered.config.key_hex, "00000000000000000000000000000000");
        let serialized = serde_json::to_value(entry).unwrap();
        assert!(serialized.get("key_hex").is_none());
        assert!(serialized.get("peer_ip").is_none());
    }

    #[test]
    fn journal_rejects_foreign_or_unbounded_resource_claims() {
        let mut entry = Entry::from_record(&record());
        entry.owner = "bad\nowner".into();
        assert!(!entry.validate());
        let mut entry = Entry::from_record(&record());
        entry.attempted_resources.insert("unknown-provider".into());
        assert!(!entry.validate());
        let mut entry = Entry::from_record(&record());
        entry.legacy_effect_pending = true;
        assert!(!entry.validate());
        entry.legacy_effect_target = Some("bad\npath".into());
        assert!(!entry.validate());
        entry.legacy_effect_target = Some("x".repeat(4097));
        assert!(!entry.validate());
    }

    #[test]
    fn inactive_legacy_effect_markers_are_readable_but_never_written_again() {
        let mut old = serde_json::to_value(Entry::from_record(&record())).unwrap();
        old["limiter_attempted"] = json!(false);
        old["limiter_executable_path"] = Value::Null;
        let entry: Entry = serde_json::from_value(old).unwrap();
        assert!(entry.validate());
        let current = serde_json::to_value(entry).unwrap();
        assert!(current.get("limiter_attempted").is_none());
        assert!(current.get("limiter_executable_path").is_none());
    }

    #[test]
    fn journal_preserves_reference_specification_and_transferred_display_operation() {
        let mut record = record();
        record.config.virtual_display_intent = Some(
            serde_json::from_value(json!({
                "identity":"00000000-0000-0000-0000-000000000042",
                "reference_id":"client-original-reference",
                "name":"Shared monitor", "width":1920,"height":1080,"refresh_millihz":60000,
                "dd_virtual_display_scale":150,"dd_activate_virtual_display":true,
                "dd_use_sunshine_virtual_display_driver":true,
                "virtual_display_mode":"shared","virtual_display_layout":"extended"
            }))
            .unwrap(),
        );
        record.virtual_display_attempted = true;
        record.display_operation_id = Some("ss-previous-owner-display".into());
        let entry = Entry::from_record(&record);
        let recovered = entry.recovery_record().unwrap();
        assert_eq!(
            super::super::virtual_display::status(&recovered),
            super::super::virtual_display::status(&record)
        );
        assert_eq!(
            super::super::virtual_display::remove(&recovered),
            super::super::virtual_display::remove(&record)
        );
        assert_eq!(recovered.display_operation(), record.display_operation());
        let restored = recovered.config.virtual_display_intent.unwrap();
        let original = record.config.virtual_display_intent.unwrap();
        assert_eq!(
            (
                restored.width,
                restored.height,
                restored.refresh_millihz,
                restored.name
            ),
            (
                original.width,
                original.height,
                original.refresh_millihz,
                original.name
            )
        );
        let encoded = serde_json::to_value(&entry).unwrap();
        assert!(encoded["virtual_display"].get("display_policy").is_none());
        for (field, invalid) in [
            ("identity", json!("not-a-uuid")),
            ("reference_id", json!("x".repeat(129))),
            ("name", json!("x".repeat(32))),
            ("width", json!(7681)),
            ("height", json!(0)),
            ("refresh_millihz", json!(0)),
        ] {
            let mut value = encoded.clone();
            value["virtual_display"][field] = invalid;
            let invalid: Entry = serde_json::from_value(value).unwrap();
            assert!(!invalid.validate(), "{field}");
            assert!(invalid.recovery_record().is_err());
        }
        let mut invalid = entry;
        invalid.display_operation_id = Some("x".repeat(129));
        assert!(!invalid.validate());
    }

    #[test]
    fn journal_compare_and_swap_advances_only_the_confirmed_revision() {
        let mut journal = LifecycleJournal::empty_for_test();
        let mut next = Document::default();
        next.entries
            .insert(record().id, Entry::from_record(&record()));
        assert_eq!(journal.set_payload(&next)["expected_revision"], 0);
        assert!(journal.commit(next.clone(), &json!({"revision":1})).is_ok());
        assert_eq!(journal.revision, 1);
        assert_eq!(journal.document, next);
        assert_eq!(
            journal.commit(next, &json!({"revision":3})),
            Err("lifecycle_journal_revision_unconfirmed")
        );
    }
}

#[cfg(test)]
mod provider_recovery_tests {
    use super::*;

    #[test]
    fn provider_roles_survive_restart_instead_of_following_new_startup_selection() {
        let config = serde_json::from_value(json!({"peer_ip":"127.0.0.1","key_hex":"00000000000000000000000000000000","key_id":0,"connect_data":0,"operation_id":"recovery"})).unwrap();
        let mut record = Record::new(7, "owner".into(), config, 30_000);
        record.providers.media_transport = "original.media/1".into();
        record.providers.control_transport = "original.control/1".into();
        record
            .attempted
            .insert(super::super::record::Resource::Media);
        let entry = Entry::from_record(&record);
        let value = serde_json::to_value(&entry).unwrap();
        let recovered = serde_json::from_value::<Entry>(value)
            .unwrap()
            .recovery_record()
            .unwrap();
        assert_eq!(
            recovered.service(super::super::record::Resource::Media),
            "original.media/1"
        );
        assert_eq!(
            recovered.service(super::super::record::Resource::Control),
            "original.control/1"
        );
    }
}
