use crate::logging::{LoggingSettings, SessionLogger};
use crate::protocol::{Action, Record, page_limit, valid_name};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

const MAX_CALLERS: usize = 64;
const MAX_RECORDS: usize = 128;
const MAX_COUNTERS: usize = 32;
const MAX_RECORD_BYTES: usize = 2048;

type Result<T> = std::result::Result<T, &'static str>;

#[derive(Default)]
struct CallerDiagnostics {
    records: VecDeque<Record>,
    sequence: u64,
    dropped_total: u64,
    counters: BTreeMap<String, u64>,
}

/// A bounded, volatile journal. The host supplies caller identity; payloads cannot.
pub struct Journal {
    callers: BTreeMap<String, CallerDiagnostics>,
    logger: SessionLogger,
    ready: bool,
    logging_configuration_revision: Option<u64>,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            callers: BTreeMap::new(),
            logger: SessionLogger::default(),
            ready: true,
            logging_configuration_revision: None,
        }
    }
}

impl Journal {
    pub(crate) fn unready() -> Self {
        Self {
            ready: false,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_logging(settings: LoggingSettings) -> (Self, Option<String>) {
        let mut journal = Self::default();
        let warning = journal.configure_logging(settings);
        (journal, warning)
    }

    pub(crate) fn configure_logging(&mut self, settings: LoggingSettings) -> Option<String> {
        let (logger, warning) = SessionLogger::open(settings);
        self.logger = logger;
        warning
    }

    /// Keep the effective policy and its revision together when a refreshed sink fails.
    pub(crate) fn apply_persisted_logging(
        &mut self,
        settings: LoggingSettings,
        revision: u64,
    ) -> std::result::Result<(), String> {
        if revision == 0 {
            return Err("host configuration revision must be positive".into());
        }
        if self
            .logging_configuration_revision
            .is_some_and(|applied| revision <= applied)
        {
            return Err("host configuration revision must advance".into());
        }
        let (logger, warning) = SessionLogger::open(settings);
        if let Some(warning) = warning {
            return Err(warning);
        }
        self.logger = logger;
        self.logging_configuration_revision = Some(revision);
        self.ready = true;
        Ok(())
    }

    pub(crate) fn set_ready(&mut self, ready: bool) {
        self.ready = ready;
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.ready
    }

    pub(crate) fn logging_configuration_revision(&self) -> Option<u64> {
        self.logging_configuration_revision
    }

    pub fn handle(&mut self, caller: &str, payload: Value) -> Value {
        let result = serde_json::from_value(payload)
            .map_err(|_| "invalid_request")
            .and_then(|action| self.execute(caller, action));
        let mut response = match result {
            Ok(response) => response,
            Err(code) => json!({"ok": false, "code": code}),
        };
        if !self.ready
            && let Some(object) = response.as_object_mut()
        {
            object.insert("ready".into(), json!(false));
        }
        response
    }

    fn execute(&mut self, caller: &str, action: Action) -> Result<Value> {
        match action {
            Action::Query { after, limit } => {
                let empty = CallerDiagnostics::default();
                self.callers
                    .get(caller)
                    .unwrap_or(&empty)
                    .query(after, limit)
            }
            Action::Counters {} => {
                let empty = CallerDiagnostics::default();
                let diagnostics = self.callers.get(caller).unwrap_or(&empty);
                Ok(json!({"ok":true,"counters":diagnostics.counters}))
            }
            Action::Record {
                level,
                target,
                message,
                fields,
            } => {
                if !self.ready {
                    return Err("not_ready");
                }
                let record = Record {
                    sequence: 0,
                    level,
                    target,
                    message,
                    fields,
                };
                validate_record(&record)?;
                let response = self.caller_mut(caller)?.append(record.clone())?;
                self.logger.write_record(caller, &record);
                Ok(response)
            }
            Action::Increment { name, amount } => {
                if !valid_name(&name) {
                    return Err("invalid_name");
                }
                self.caller_mut(caller)?.increment(name, amount)
            }
            Action::Status {} => Ok(json!({
                "ok":true,
                "ready":self.ready,
                "logging_configuration_revision":self.logging_configuration_revision,
                "logging_sink":self.logger.health(),
            })),
        }
    }

    fn caller_mut(&mut self, caller: &str) -> Result<&mut CallerDiagnostics> {
        if caller.is_empty() || caller.len() > 128 {
            return Err("invalid_caller");
        }
        if !self.callers.contains_key(caller) && self.callers.len() >= MAX_CALLERS {
            return Err("caller_limit");
        }
        Ok(self.callers.entry(caller.to_owned()).or_default())
    }
}

fn validate_record(record: &Record) -> Result<()> {
    if !valid_name(&record.target)
        || record.message.len() > 512
        || record.fields.len() > 8
        || record
            .fields
            .iter()
            .any(|(key, value)| !valid_name(key) || value.len() > 256)
    {
        return Err("invalid_record");
    }
    // Reserve nineteen bytes for growing sequence 0 to a twenty-digit u64.
    if serde_json::to_vec(record)
        .map_err(|_| "invalid_record")?
        .len()
        + 19
        > MAX_RECORD_BYTES
    {
        return Err("record_too_large");
    }
    Ok(())
}

impl CallerDiagnostics {
    fn append(&mut self, mut record: Record) -> Result<Value> {
        let sequence = self.sequence.checked_add(1).ok_or("sequence_exhausted")?;
        record.sequence = sequence;
        if self.records.len() == MAX_RECORDS {
            self.records.pop_front();
            self.dropped_total += 1;
        }
        self.sequence = sequence;
        self.records.push_back(record);
        Ok(json!({"ok":true,"sequence":sequence,"dropped_total":self.dropped_total}))
    }

    fn query(&self, after: u64, limit: usize) -> Result<Value> {
        if !(1..=page_limit()).contains(&limit) {
            return Err("invalid_limit");
        }
        if after > self.sequence {
            return Err("invalid_cursor");
        }
        let oldest_sequence = self.records.front().map(|record| record.sequence);
        let resync_required = oldest_sequence.is_some_and(|oldest| after < oldest - 1);
        let records: Vec<_> = self
            .records
            .iter()
            .filter(|record| record.sequence > after)
            .take(limit)
            .collect();
        let next_cursor = records.last().map_or(after, |record| record.sequence);
        Ok(json!({
            "ok":true, "records":records, "next_cursor":next_cursor,
            "has_more":next_cursor < self.sequence, "latest_sequence":self.sequence,
            "oldest_sequence":oldest_sequence, "dropped_total":self.dropped_total,
            "resync_required":resync_required,
        }))
    }

    fn increment(&mut self, name: String, amount: u64) -> Result<Value> {
        if !self.counters.contains_key(&name) && self.counters.len() >= MAX_COUNTERS {
            return Err("counter_limit");
        }
        let previous = self.counters.get(&name).copied().unwrap_or(0);
        let value = previous.checked_add(amount).ok_or("counter_overflow")?;
        self.counters.insert(name.clone(), value);
        Ok(json!({"ok":true,"name":name,"value":value}))
    }
}

#[cfg(test)]
mod hydration_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_are_rejected_until_logging_policy_is_ready() {
        let mut journal = Journal::unready();

        let response = journal.handle(
            "capture",
            json!({
                "action":"record",
                "level":"info",
                "target":"capture",
                "message":"early"
            }),
        );

        assert_eq!(
            response,
            json!({"ok":false,"code":"not_ready","ready":false})
        );
        assert!(journal.callers.is_empty());
        assert_eq!(
            journal.handle("capture", json!({"action":"status"})),
            json!({"ok":true,"ready":false,"logging_configuration_revision":null,
                "logging_sink":{"state":"disabled","failure":null}})
        );
    }

    #[test]
    fn adopted_configuration_allows_records_and_reports_readiness() {
        let mut journal = Journal::unready();
        let settings = LoggingSettings::from_value(json!({})).unwrap();

        journal.configure_logging(settings);
        journal.set_ready(true);

        assert!(journal.is_ready());
        assert_eq!(
            journal.handle("capture", json!({"action":"status"})),
            json!({"ok":true,"ready":true,"logging_configuration_revision":null,
                "logging_sink":{"state":"disabled","failure":null}})
        );
        assert_eq!(
            journal.handle(
                "capture",
                json!({
                    "action":"record",
                    "level":"info",
                    "target":"capture",
                    "message":"ready"
                }),
            ),
            json!({"ok":true,"sequence":1,"dropped_total":0})
        );
    }

    #[test]
    fn successful_refresh_advances_revision_and_failed_sink_keeps_effective_policy() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-diagnostics-status-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut journal = Journal::unready();
        let settings = |path: &str| {
            LoggingSettings::from_value(json!({
                "min_log_level":2,
                "log_path":path,
                "state_root":root,
            }))
            .unwrap()
        };

        journal
            .apply_persisted_logging(settings("first.log"), 3)
            .unwrap();
        assert_eq!(
            journal.handle("capture", json!({"action":"status"})),
            json!({
                "ok":true,"ready":true,"logging_configuration_revision":3,
                "logging_sink":{"state":"healthy","failure":null}
            })
        );
        assert!(
            journal
                .apply_persisted_logging(settings("stale.log"), 3)
                .is_err()
        );
        assert_eq!(
            journal.handle("capture", json!({"action":"status"}))["logging_configuration_revision"],
            3
        );

        let blocked = root.join("blocked");
        std::fs::write(&blocked, b"file blocks a log directory").unwrap();
        assert!(
            journal
                .apply_persisted_logging(settings("blocked/failed.log"), 4)
                .is_err()
        );
        assert_eq!(
            journal.handle("capture", json!({"action":"status"}))["logging_configuration_revision"],
            3
        );
        journal.handle("capture", json!({
            "action":"record","level":"error","target":"capture","message":"after failed refresh"
        }));
        let first_log = std::fs::read_dir(root.join("logs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("first-")
            })
            .unwrap();
        assert!(
            std::fs::read_to_string(first_log)
                .unwrap()
                .contains("after failed refresh")
        );

        journal
            .apply_persisted_logging(settings("second.log"), 4)
            .unwrap();
        assert_eq!(
            journal.handle("capture", json!({"action":"status"}))["logging_configuration_revision"],
            4
        );
        drop(journal);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_sink_failure_is_visible_and_recovers_after_restart() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-diagnostics-sink-health-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let settings = || {
            LoggingSettings::from_value(json!({
                "min_log_level":2,
                "log_path":"capture.log",
                "state_root":root,
            }))
            .unwrap()
        };
        let record = |message| {
            json!({
                "action":"record","level":"info","target":"capture","message":message
            })
        };
        let status = |journal: &mut Journal| journal.handle("capture", json!({"action":"status"}));

        let mut journal = Journal::unready();
        journal.apply_persisted_logging(settings(), 7).unwrap();
        assert_eq!(status(&mut journal)["logging_sink"]["state"], "healthy");

        journal.logger.close_sink_for_test();
        assert_eq!(
            journal.handle("capture", record("retained on disk failure"))["sequence"],
            1
        );
        assert_eq!(status(&mut journal)["logging_configuration_revision"], 7);
        assert_eq!(
            status(&mut journal)["logging_sink"],
            json!({"state":"failed","failure":"write"})
        );
        assert_eq!(
            journal.handle("capture", json!({"action":"query"}))["records"][0]["message"],
            "retained on disk failure"
        );

        assert_eq!(
            journal.handle("capture", record("written after recovery"))["sequence"],
            2
        );
        assert_eq!(
            status(&mut journal)["logging_sink"],
            json!({"state":"healthy","failure":null})
        );
        let log_path = journal.logger.active_path_for_test().to_path_buf();
        let contents = std::fs::read_to_string(log_path).unwrap();
        assert!(contents.contains("written after recovery"));
        assert!(!contents.contains("retained on disk failure"));

        drop(journal);
        let mut restarted = Journal::unready();
        restarted.apply_persisted_logging(settings(), 7).unwrap();
        assert_eq!(status(&mut restarted)["logging_configuration_revision"], 7);
        assert_eq!(
            status(&mut restarted)["logging_sink"],
            json!({"state":"healthy","failure":null})
        );
        assert_eq!(
            restarted.handle("capture", record("written after restart"))["sequence"],
            1
        );
        assert_eq!(
            restarted.handle("capture", json!({"action":"query"}))["records"][0]["message"],
            "written after restart"
        );
        drop(restarted);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blocked_log_directory_recovers_without_changing_the_logging_policy() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-diagnostics-blocked-directory-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let blocked = root.join("blocked");
        std::fs::write(&blocked, b"file blocks a log directory").unwrap();
        let settings = LoggingSettings::from_value(json!({
            "min_log_level":3,
            "log_path":"blocked/capture.log",
            "state_root":root,
        }))
        .unwrap();
        let mut journal = Journal::default();
        assert!(journal.configure_logging(settings).is_some());
        let record = |level, message| json!({"action":"record","level":level,"target":"capture","message":message});
        let status = |journal: &mut Journal| journal.handle("capture", json!({"action":"status"}));

        assert_eq!(
            status(&mut journal)["logging_sink"],
            json!({"state":"failed","failure":"open"})
        );
        assert_eq!(
            journal.handle("capture", record("info", "below threshold"))["sequence"],
            1
        );
        assert_eq!(
            status(&mut journal)["logging_sink"],
            json!({"state":"failed","failure":"open"})
        );
        assert_eq!(
            journal.handle("capture", record("warn", "still blocked"))["sequence"],
            2
        );
        assert_eq!(
            status(&mut journal)["logging_sink"],
            json!({"state":"failed","failure":"open"})
        );

        std::fs::remove_file(blocked).unwrap();
        assert_eq!(
            journal.handle("capture", record("warn", "after recovery"))["sequence"],
            3
        );
        assert_eq!(
            status(&mut journal)["logging_sink"],
            json!({"state":"healthy","failure":null})
        );
        let log_path = journal.logger.active_path_for_test().to_path_buf();
        let contents = std::fs::read_to_string(log_path).unwrap();
        assert!(contents.contains("after recovery"));
        assert!(!contents.contains("still blocked"));
        assert!(!contents.contains("below threshold"));
        assert_eq!(
            journal.handle("capture", json!({"action":"query"}))["records"]
                .as_array()
                .unwrap()
                .len(),
            3
        );

        drop(journal);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn queries_and_counter_actions_remain_available_while_unready() {
        let mut journal = Journal::unready();

        assert_eq!(
            journal.handle("capture", json!({"action":"query"}))["ready"],
            false
        );
        assert_eq!(
            journal.handle("capture", json!({"action":"counters"}))["ready"],
            false
        );
        assert_eq!(
            journal.handle(
                "capture",
                json!({"action":"increment","name":"frames","amount":1})
            ),
            json!({"ok":true,"name":"frames","value":1,"ready":false})
        );
        assert_eq!(
            journal.handle("capture", json!({"action":"counters"})),
            json!({"ok":true,"counters":{"frames":1},"ready":false})
        );
    }
}

#[cfg(test)]
mod tests;
