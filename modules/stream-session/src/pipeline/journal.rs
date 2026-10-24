//! Caller-scoped write-ahead journal for interrupted stream-session cleanup.
//!
//! The state module owns the private runtime state directory. This journal stores
//! only stable session identity and attempted-resource flags; never stream keys,
//! peer addresses, negotiated configuration, resource handles, or lease tokens.
use super::{record::Record, rpc};
use crate::model::PrepareConfig;
use amaterasu_module_sdk::{ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const KEY: &str = "stream-session/lifecycle/v1";
const FORMAT_VERSION: u32 = 1;
const MAX_ENTRIES: usize = 2;

pub(super) type Send<'a> = dyn FnMut(amaterasu_module_sdk::Command) -> ModuleResult<Response> + 'a;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub record_id: u64,
    pub owner: String,
    pub attempted_resources: BTreeSet<String>,
    pub input_attempted: bool,
    pub display_attempted: bool,
    pub limiter_attempted: bool,
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
            record_id: record.id,
            owner: record.owner.clone(),
            attempted_resources: record
                .attempted
                .iter()
                .map(|resource| resource.name().to_owned())
                .collect(),
            input_attempted: record.input_attempted,
            display_attempted: record.display_attempted,
            limiter_attempted: record.limiter_attempted,
        }
    }

    pub fn validate(&self) -> bool {
        self.record_id != 0
            && !self.owner.is_empty()
            && self.owner.len() <= 128
            && !self.owner.chars().any(char::is_control)
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
        record.attempted = self
            .attempted_resources
            .iter()
            .map(|name| resource_from_name(name).ok_or("invalid_lifecycle_journal_entry"))
            .collect::<Result<_, _>>()?;
        record.input_attempted = self.input_attempted;
        record.input_lookup_required = self.input_attempted;
        record.display_attempted = self.display_attempted;
        record.limiter_attempted = self.limiter_attempted;
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
        })
    }

    pub fn empty_for_test() -> Self {
        Self {
            nonce: "test".into(),
            loaded: true,
            failed: false,
            revision: 0,
            document: Document::default(),
            load: None,
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.document.entries.values()
    }

    pub fn matches_record(&self, record: &Record) -> bool {
        self.document
            .entries
            .get(&record.id)
            .is_some_and(|entry| *entry == Entry::from_record(record))
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
        if self.load.is_none() {
            self.load = Some(rpc::Pending::new(
                "amaterasu.state/1",
                json!({"action":"get","key":KEY}),
                format!("ss-journal-{}-load", self.nonce),
                None,
            ));
        }
        let progress = self
            .load
            .as_mut()
            .expect("load was created")
            .poll(send, now);
        match progress {
            rpc::Progress::Waiting => LoadProgress::Waiting,
            rpc::Progress::Unresolved | rpc::Progress::Failed => {
                self.failed = true;
                self.load = None;
                LoadProgress::Failed
            }
            rpc::Progress::Result(payload) => {
                let result = self.accept_load(&payload);
                let released = self
                    .load
                    .as_mut()
                    .expect("load result remains owned")
                    .release_result(send);
                self.load = None;
                if result.is_err() || !released {
                    self.failed = true;
                    LoadProgress::Failed
                } else {
                    self.loaded = true;
                    LoadProgress::Ready
                }
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
        &self,
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
