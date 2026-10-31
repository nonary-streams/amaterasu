use crate::model::{
    HistorySettings, HistoryStatus, MAX_ACTIVE_LIMIT, MAX_DETAIL_EVENTS, MAX_DETAIL_SAMPLES,
    MAX_SESSION_LIMIT, MAX_STORED_EVENTS, MAX_STORED_SAMPLES, PruneReport, SessionDetail,
    SessionEvent, SessionMetadata, SessionSample, SessionSummary, validate_text,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod timeline;

const SCHEMA_VERSION: i64 = 3;
const SECONDS_PER_DAY: i64 = 86_400;
const PRUNE_INTERVAL_SECONDS: i64 = 3_600;
const BYTES_PER_MEBIBYTE: u64 = 1024 * 1024;
// The broker rejects completed response payloads above this serialized size.
const MAX_DETAIL_RESPONSE_BYTES: usize = 16_384;
const ACTIVITY_RESERVATION_SECONDS: i64 = 300;
const MAX_ACTIVITY_IDS: usize = 4096;

struct PendingActivity {
    expires_at_unix: i64,
    reserved_at: Instant,
}

#[derive(Debug)]
pub enum HistoryError {
    Invalid(&'static str),
    Database,
    NotFound,
    ActiveSession,
    RecordingDisabled,
    HistoryChanged,
}

pub struct SessionPage {
    pub sessions: Vec<SessionSummary>,
    pub next_offset: Option<usize>,
}

impl HistoryError {
    pub fn is_database(&self) -> bool {
        matches!(self, Self::Database)
    }
}

impl From<rusqlite::Error> for HistoryError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}

/// The connection is privately owned and every mutation requires `&mut self`.
/// Module request dispatch is single-threaded, so SQLite writes have one owner
/// and preserve admission order without a separate writer queue.
pub struct HistoryStore {
    path: PathBuf,
    connection: Connection,
    writer_open: bool,
    recovered: bool,
    #[cfg(test)]
    fail_next_read_only_open: bool,
    settings: HistorySettings,
    configuration_ready: bool,
    desired_revision: Option<u64>,
    applied_revision: Option<u64>,
    effective_enabled: bool,
    active_streams: BTreeSet<String>,
    pending_activities: BTreeMap<String, PendingActivity>,
    terminal_activities: BTreeSet<String>,
    terminal_order: VecDeque<String>,
    failed_operations: u64,
    last_prune: PruneReport,
    next_prune_at_unix: Option<i64>,
}

impl HistoryStore {
    pub fn open(path: &Path, _now_unix: i64) -> Result<Self, HistoryError> {
        if !path.is_absolute() {
            return Err(HistoryError::Invalid("database path must be absolute"));
        }
        let parent = path.parent().ok_or(HistoryError::Invalid(
            "database path has no parent directory",
        ))?;
        amaterasu_storage::create_private_directory(parent).map_err(|_| HistoryError::Database)?;

        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
        apply_schema(&mut connection)?;
        let store = Self {
            path: path.to_path_buf(),
            settings: HistorySettings::default(),
            configuration_ready: false,
            desired_revision: None,
            applied_revision: None,
            effective_enabled: false,
            connection,
            writer_open: true,
            recovered: false,
            #[cfg(test)]
            fail_next_read_only_open: false,
            active_streams: BTreeSet::new(),
            pending_activities: BTreeMap::new(),
            terminal_activities: BTreeSet::new(),
            terminal_order: VecDeque::new(),
            failed_operations: 0,
            last_prune: empty_prune_report(),
            next_prune_at_unix: None,
        };
        Ok(store)
    }

    pub fn settings(&self) -> (&HistorySettings, bool) {
        (&self.settings, self.effective_enabled)
    }

    /// Returns the previous SQLite-owned policy so startup can import it into
    /// the central product document before deleting the obsolete table.
    pub fn legacy_settings(&self) -> Result<Option<HistorySettings>, HistoryError> {
        if !table_exists(&self.connection, "history_settings")? {
            return Ok(None);
        }
        let legacy = self
            .connection
            .query_row(
                "SELECT enabled, ttl_days, db_size_limit_mb
                 FROM history_settings WHERE singleton = 1",
                [],
                |row| {
                    Ok(HistorySettings {
                        enabled: row.get(0)?,
                        ttl_days: row.get(1)?,
                        db_size_limit_mb: row.get(2)?,
                    })
                },
            )
            .optional()?;
        if let Some(settings) = &legacy {
            settings.validate().map_err(HistoryError::Invalid)?;
        }
        Ok(legacy)
    }

    /// Marks the policy loaded from `amaterasu.configuration/1` as active.
    /// The revision is owned by the configuration service, not SQLite.
    #[cfg(test)]
    pub fn apply_configuration(
        &mut self,
        settings: HistorySettings,
        now_unix: i64,
    ) -> Result<(), HistoryError> {
        self.apply_configuration_inner(settings, None, now_unix)
    }

    pub fn apply_configuration_with_revision(
        &mut self,
        settings: HistorySettings,
        revision: u64,
        now_unix: i64,
    ) -> Result<(), HistoryError> {
        self.apply_configuration_inner(settings, Some(revision), now_unix)
    }

    fn apply_configuration_inner(
        &mut self,
        settings: HistorySettings,
        revision: Option<u64>,
        now_unix: i64,
    ) -> Result<(), HistoryError> {
        settings.validate().map_err(HistoryError::Invalid)?;
        self.settings = settings;
        self.configuration_ready = true;
        self.desired_revision = revision;
        // Keep the recording decision stable for every stream in the current
        // cohort. Retention reads the new settings immediately below.
        self.expire_stream_activities(now_unix);
        if self.activity_cohort_drained() {
            self.transition_writer(now_unix)?;
        }
        if self.effective_enabled {
            self.prune_best_effort(now_unix);
        }
        self.next_prune_at_unix = Some(now_unix.saturating_add(PRUNE_INTERVAL_SECONDS));
        Ok(())
    }

    /// Periodic retention also runs when no further sessions end. Failed
    /// attempts count as degraded operations and retry at the next interval.
    pub fn prune_if_due(&mut self, now_unix: i64) {
        self.expire_stream_activities(now_unix);
        if self.configuration_ready
            && (self.writer_open != self.settings.enabled
                || self.effective_enabled != self.settings.enabled
                || self.applied_revision != self.desired_revision)
        {
            // A failed connection transition is retried without changing the
            // policy or dropping admitted writes.
            if self.activity_cohort_drained() && self.transition_writer(now_unix).is_err() {
                self.record_failure();
            }
        }
        let Some(next_prune_at_unix) = self.next_prune_at_unix else {
            return;
        };
        if now_unix < next_prune_at_unix {
            return;
        }
        self.next_prune_at_unix = Some(now_unix.saturating_add(PRUNE_INTERVAL_SECONDS));
        if self.effective_enabled
            && (self.settings.ttl_days > 0 || self.settings.db_size_limit_mb > 0)
        {
            self.prune_best_effort(now_unix);
        }
    }

    /// Removes the v1 local policy only after the central document is known
    /// durable. Keeping this separate makes failed imports restart-safe.
    /// Returns false when cleanup must wait for an enabled writer. A saved
    /// policy must never depend on this obsolete-table maintenance succeeding.
    pub fn discard_legacy_settings(&mut self) -> Result<bool, HistoryError> {
        if !self.writer_open {
            return Ok(!table_exists(&self.connection, "history_settings")?);
        }
        if table_exists(&self.connection, "history_settings")? {
            self.connection
                .execute_batch("DROP TABLE history_settings")?;
        }
        Ok(true)
    }

    /// Reserve the current recording policy while a stream is being launched.
    /// Repeated begin calls keep the first expiry; only volatile state is used.
    pub fn begin_stream_activity(
        &mut self,
        session_id: &str,
        now_unix: i64,
    ) -> Result<bool, HistoryError> {
        validate_activity_id(session_id)?;
        self.expire_stream_activities(now_unix);
        // Expiry can drain the previous cohort. Apply a saved toggle before
        // admitting this launch, or it would inherit the prior cohort's policy.
        self.transition_drained_writer(now_unix)?;
        if self.terminal_activities.contains(session_id) {
            return Err(HistoryError::Invalid("stream activity has already ended"));
        }
        if !self.configuration_ready {
            return Ok(false);
        }
        if self.active_streams.contains(session_id)
            || self.pending_activities.contains_key(session_id)
        {
            return Ok(true);
        }
        if self.pending_activities.len() >= MAX_ACTIVITY_IDS {
            return Err(HistoryError::Invalid("too many pending stream activities"));
        }
        self.pending_activities.insert(
            session_id.to_owned(),
            PendingActivity {
                expires_at_unix: now_unix.saturating_add(ACTIVITY_RESERVATION_SECONDS),
                reserved_at: Instant::now(),
            },
        );
        Ok(true)
    }

    /// Ends a pending launch without creating a history event. An attached
    /// active stream is left for session_end to close in the usual order.
    pub fn end_stream_activity(
        &mut self,
        session_id: &str,
        now_unix: i64,
    ) -> Result<bool, HistoryError> {
        validate_activity_id(session_id)?;
        self.expire_stream_activities(now_unix);
        if self.terminal_activities.contains(session_id) {
            return Ok(false);
        }
        self.pending_activities.remove(session_id);
        self.mark_activity_terminal(session_id);
        self.finish_drained_transition(now_unix);
        Ok(true)
    }

    #[cfg(test)]
    pub fn begin_session(
        &mut self,
        metadata: &SessionMetadata,
        started_at_unix: i64,
    ) -> Result<bool, HistoryError> {
        self.begin_session_at(metadata, started_at_unix, started_at_unix)
    }

    pub fn begin_session_at(
        &mut self,
        metadata: &SessionMetadata,
        started_at_unix: i64,
        now_unix: i64,
    ) -> Result<bool, HistoryError> {
        metadata.validate().map_err(HistoryError::Invalid)?;
        self.expire_stream_activities(now_unix);
        // A direct session begin can also be the first call after a pending
        // launch expires, so it must start under the newly effective policy.
        self.transition_drained_writer(now_unix)?;
        if self.terminal_activities.contains(&metadata.session_id) {
            return Err(HistoryError::Invalid("stream activity has already ended"));
        }
        if self.active_streams.contains(&metadata.session_id) {
            return Ok(false);
        }
        if session_exists(&self.connection, &metadata.session_id)? {
            return Err(HistoryError::Invalid(
                "session_id has already been recorded",
            ));
        }

        if !self.effective_enabled {
            self.active_streams.insert(metadata.session_id.clone());
            self.pending_activities.remove(&metadata.session_id);
            return Ok(false);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO sessions (
                session_id, protocol, client_name, device_name, app_name, width, height,
                target_fps, target_bitrate_kbps, codec, started_at_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                metadata.session_id,
                metadata.protocol,
                metadata.client_name,
                metadata.device_name,
                metadata.app_name,
                i64::from(metadata.width),
                i64::from(metadata.height),
                i64::from(metadata.target_fps),
                i64::from(metadata.target_bitrate_kbps),
                metadata.codec,
                started_at_unix,
            ],
        )?;
        transaction.execute(
            "INSERT INTO events (session_id, recorded_at_unix, event_type, payload)
             VALUES (?1, ?2, 'stream_started', NULL)",
            params![metadata.session_id, started_at_unix],
        )?;
        trim_events(&transaction, &metadata.session_id)?;
        transaction.commit()?;
        self.active_streams.insert(metadata.session_id.clone());
        self.pending_activities.remove(&metadata.session_id);
        Ok(true)
    }

    /// Preserve the producer's end time in the record, but evaluate retention
    /// against the module's processing time so delayed updates cannot shift TTL.
    pub fn end_session(
        &mut self,
        session_id: &str,
        ended_at_unix: i64,
        retention_now_unix: i64,
    ) -> Result<bool, HistoryError> {
        validate_text(
            session_id,
            1,
            crate::model::MAX_SESSION_ID_BYTES,
            "session_id",
        )
        .map_err(HistoryError::Invalid)?;
        let was_active = self.active_streams.contains(session_id);
        let mut ended = false;
        if self.effective_enabled {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = transaction.execute(
                "UPDATE sessions
                 SET ended_at_unix = ?1,
                     duration_seconds = max(?1 - started_at_unix, 0)
                 WHERE session_id = ?2 AND ended_at_unix IS NULL",
                params![ended_at_unix, session_id],
            )?;
            if changed > 0 {
                transaction.execute(
                    "INSERT INTO events (session_id, recorded_at_unix, event_type, payload)
                     VALUES (?1, ?2, 'stream_ended', NULL)",
                    params![session_id, ended_at_unix],
                )?;
                trim_events(&transaction, session_id)?;
                ended = true;
            }
            transaction.commit()?;
        }
        if was_active {
            self.active_streams.remove(session_id);
        }
        // Even an end that raced ahead of begin is final for this ID. A late
        // begin must not resurrect a session after its terminal signal.
        self.pending_activities.remove(session_id);
        self.mark_activity_terminal(session_id);
        if self.effective_enabled && (self.settings.enabled || ended) {
            self.prune_best_effort(retention_now_unix);
        }
        if self.transition_drained_writer(retention_now_unix).is_err() {
            self.record_failure();
        }
        Ok(ended)
    }

    pub fn record_sample(&mut self, sample: &SessionSample) -> Result<bool, HistoryError> {
        sample.validate().map_err(HistoryError::Invalid)?;
        if !self.effective_enabled || !is_active_record(&self.connection, &sample.session_id)? {
            return Ok(false);
        }
        let frames_sent = i64::try_from(sample.frames_sent)
            .map_err(|_| HistoryError::Invalid("frames_sent exceeds SQLite integer range"))?;
        let bytes_sent = i64::try_from(sample.bytes_sent)
            .map_err(|_| HistoryError::Invalid("bytes_sent exceeds SQLite integer range"))?;
        let dropped_frames = i64::try_from(sample.dropped_frames)
            .map_err(|_| HistoryError::Invalid("dropped_frames exceeds SQLite integer range"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO samples (
                session_id, recorded_at_unix, frames_sent, bytes_sent, dropped_frames,
                actual_fps, actual_bitrate_kbps, encode_latency_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                sample.session_id,
                sample.recorded_at_unix,
                frames_sent,
                bytes_sent,
                dropped_frames,
                sample.actual_fps,
                sample.actual_bitrate_kbps,
                sample.encode_latency_ms,
            ],
        )?;
        trim_samples(&transaction, &sample.session_id)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn record_event(&mut self, event: &SessionEvent) -> Result<bool, HistoryError> {
        event.validate().map_err(HistoryError::Invalid)?;
        if !self.effective_enabled || !is_active_record(&self.connection, &event.session_id)? {
            return Ok(false);
        }
        let payload = event
            .payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| HistoryError::Database)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO events (session_id, recorded_at_unix, event_type, payload)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                event.session_id,
                event.recorded_at_unix,
                event.event_type,
                payload
            ],
        )?;
        trim_events(&transaction, &event.session_id)?;
        transaction.commit()?;
        Ok(true)
    }

    #[cfg(test)]
    pub fn list_sessions(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionSummary>, HistoryError> {
        Ok(self.list_sessions_page(limit, offset)?.sessions)
    }

    pub fn list_sessions_page(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<SessionPage, HistoryError> {
        self.session_page(limit, offset)
    }

    pub fn session_detail(&self, session_id: &str) -> Result<SessionDetail, HistoryError> {
        let session = read_summary(&self.connection, session_id)?.ok_or(HistoryError::NotFound)?;
        let sample_count = child_count(&self.connection, "samples", session_id)?;
        let event_count = child_count(&self.connection, "events", session_id)?;
        let samples = read_samples(&self.connection, session_id)?;
        let events = read_events(&self.connection, session_id)?;
        let mut detail = SessionDetail {
            session,
            sample_count,
            event_count,
            samples_truncated: sample_count > samples.len(),
            events_truncated: event_count > events.len(),
            samples,
            events,
        };
        bound_detail_response(&mut detail)?;
        Ok(detail)
    }

    #[cfg(test)]
    pub fn active_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, HistoryError> {
        Ok(self.active_sessions_page(limit, 0)?.sessions)
    }

    pub fn active_sessions_page(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<SessionPage, HistoryError> {
        if !(1..=MAX_ACTIVE_LIMIT).contains(&limit) {
            return Err(HistoryError::Invalid("session page limit is out of range"));
        }
        if i64::try_from(offset).is_err() {
            return Err(HistoryError::Invalid("offset exceeds SQLite integer range"));
        }
        if self.active_streams.is_empty() {
            return bound_session_page(Vec::new(), offset, false);
        }
        // Persisted rows with no end time may belong to a process that died
        // while recording was enabled. Recovery is deliberately deferred while
        // disabled, so only sessions admitted in this process are live.
        let mut statement = self.connection.prepare(
            "SELECT session_id, protocol, client_name, device_name, app_name, width, height,
                    target_fps, target_bitrate_kbps, codec, started_at_unix, ended_at_unix,
                    duration_seconds
             FROM sessions WHERE ended_at_unix IS NULL
             ORDER BY started_at_unix DESC, session_id DESC",
        )?;
        let mut rows = statement.query([])?;
        let mut matched = 0usize;
        let mut sessions = Vec::with_capacity(limit);
        while let Some(row) = rows.next()? {
            let session = summary_from_row(row)?;
            if !self.active_streams.contains(&session.session_id) {
                continue;
            }
            if matched >= offset {
                if sessions.len() == limit {
                    return bound_session_page(sessions, offset, true);
                }
                sessions.push(session);
            }
            matched = matched.saturating_add(1);
        }
        bound_session_page(sessions, offset, false)
    }

    fn session_page(&self, limit: usize, offset: usize) -> Result<SessionPage, HistoryError> {
        if !(1..=MAX_SESSION_LIMIT).contains(&limit) {
            return Err(HistoryError::Invalid("session page limit is out of range"));
        }
        let offset_sql = i64::try_from(offset)
            .map_err(|_| HistoryError::Invalid("offset exceeds SQLite integer range"))?;
        let sql = "SELECT session_id, protocol, client_name, device_name, app_name, width, height,
                    target_fps, target_bitrate_kbps, codec, started_at_unix, ended_at_unix,
                    duration_seconds
             FROM sessions
             ORDER BY started_at_unix DESC, session_id DESC LIMIT ?1 OFFSET ?2";
        let mut statement = self.connection.prepare(sql)?;
        let rows =
            statement.query_map(params![(limit + 1) as i64, offset_sql], summary_from_row)?;
        let mut sessions = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        bound_session_page(sessions, offset, has_more)
    }

    pub fn delete_session(&mut self, session_id: &str) -> Result<(), HistoryError> {
        if !self.effective_enabled {
            return Err(HistoryError::RecordingDisabled);
        }
        validate_text(
            session_id,
            1,
            crate::model::MAX_SESSION_ID_BYTES,
            "session_id",
        )
        .map_err(HistoryError::Invalid)?;
        let end_time = self
            .connection
            .query_row(
                "SELECT ended_at_unix FROM sessions WHERE session_id = ?1",
                [session_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .ok_or(HistoryError::NotFound)?;
        if end_time.is_none() {
            return Err(HistoryError::ActiveSession);
        }
        let changed = self.connection.execute(
            "DELETE FROM sessions WHERE session_id = ?1 AND ended_at_unix IS NOT NULL",
            [session_id],
        )?;
        if changed == 0 {
            return Err(HistoryError::NotFound);
        }
        Ok(())
    }

    pub fn prune(&mut self, now_unix: i64) -> Result<PruneReport, HistoryError> {
        if !self.configuration_ready || !self.effective_enabled {
            return Ok(self.last_prune.clone());
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut deleted = 0usize;
        if self.settings.ttl_days > 0 {
            let age_seconds = i64::from(self.settings.ttl_days)
                .checked_mul(SECONDS_PER_DAY)
                .ok_or(HistoryError::Invalid("ttl_days exceeds supported range"))?;
            let cutoff = now_unix.saturating_sub(age_seconds);
            deleted += transaction.execute(
                "DELETE FROM sessions WHERE ended_at_unix IS NOT NULL AND ended_at_unix < ?1",
                [cutoff],
            )?;
        }

        let limit_bytes = (self.settings.db_size_limit_mb as u64) * BYTES_PER_MEBIBYTE;
        let mut live_bytes = estimate_live_bytes(&transaction)?;
        if limit_bytes > 0 {
            while live_bytes > limit_bytes {
                let oldest: Option<String> = transaction
                    .query_row(
                        "SELECT session_id FROM sessions
                         WHERE ended_at_unix IS NOT NULL
                         ORDER BY ended_at_unix ASC, session_id ASC LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(oldest) = oldest else {
                    break;
                };
                deleted += transaction.execute(
                    "DELETE FROM sessions WHERE session_id = ?1 AND ended_at_unix IS NOT NULL",
                    [oldest],
                )?;
                live_bytes = estimate_live_bytes(&transaction)?;
            }
        }
        let size_limit_unmet = limit_bytes > 0 && live_bytes > limit_bytes;
        transaction.commit()?;
        let report = PruneReport {
            sessions_deleted: deleted,
            live_bytes,
            size_limit_bytes: limit_bytes,
            size_limit_unmet,
        };
        self.last_prune = report.clone();
        Ok(report)
    }

    pub fn status(&self) -> Result<HistoryStatus, HistoryError> {
        Ok(HistoryStatus {
            available: true,
            configuration_ready: self.configuration_ready,
            applied_revision: self.applied_revision,
            enabled: self.settings.enabled,
            effective_enabled: self.effective_enabled,
            degraded: !self.configuration_ready
                || self.failed_operations > 0
                || self.last_prune.size_limit_unmet,
            failed_operations: self.failed_operations,
            active_streams: self.active_streams.len(),
            pending_activities: self.pending_activities.len(),
            sessions: table_count(&self.connection, "sessions")?,
            samples: table_count(&self.connection, "samples")?,
            events: table_count(&self.connection, "events")?,
            live_bytes: estimate_live_bytes(&self.connection)?,
            last_prune: self.last_prune.clone(),
        })
    }

    pub fn record_failure(&mut self) {
        self.failed_operations = self.failed_operations.saturating_add(1);
    }

    fn activity_cohort_drained(&self) -> bool {
        self.active_streams.is_empty() && self.pending_activities.is_empty()
    }

    fn expire_stream_activities(&mut self, now_unix: i64) {
        let now_monotonic = Instant::now();
        let expired: Vec<_> = self
            .pending_activities
            .iter()
            .filter(|(_, activity)| {
                now_unix >= activity.expires_at_unix
                    || now_monotonic.saturating_duration_since(activity.reserved_at)
                        >= Duration::from_secs(ACTIVITY_RESERVATION_SECONDS as u64)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for session_id in expired {
            self.pending_activities.remove(&session_id);
            self.mark_activity_terminal(&session_id);
        }
    }

    fn mark_activity_terminal(&mut self, session_id: &str) {
        if self.terminal_activities.insert(session_id.to_owned()) {
            self.terminal_order.push_back(session_id.to_owned());
        }
        while self.terminal_order.len() > MAX_ACTIVITY_IDS {
            if let Some(oldest) = self.terminal_order.pop_front() {
                self.terminal_activities.remove(&oldest);
            }
        }
    }

    fn finish_drained_transition(&mut self, now_unix: i64) {
        if self.transition_drained_writer(now_unix).is_err() {
            self.record_failure();
        }
    }

    fn transition_drained_writer(&mut self, now_unix: i64) -> Result<(), HistoryError> {
        if self.configuration_ready
            && self.activity_cohort_drained()
            && (self.writer_open != self.settings.enabled
                || self.effective_enabled != self.settings.enabled
                || self.applied_revision != self.desired_revision)
        {
            self.transition_writer(now_unix)?;
        }
        Ok(())
    }

    fn prune_best_effort(&mut self, now_unix: i64) {
        if self.configuration_ready && self.effective_enabled && self.prune(now_unix).is_err() {
            self.record_failure();
        }
    }

    /// The single-threaded service has already committed every admitted write
    /// when this runs. Replace the write connection with a read-only connection
    /// only after the last active stream has ended. If opening the replacement
    /// fails, retain the current connection and retry on a later tick.
    fn transition_writer(&mut self, now_unix: i64) -> Result<(), HistoryError> {
        if self.settings.enabled == self.writer_open {
            if self.settings.enabled && !self.recovered {
                recover_existing_rows(&mut self.connection, now_unix)?;
                self.recovered = true;
            }
            self.effective_enabled = self.settings.enabled;
            self.applied_revision = self.desired_revision;
            return Ok(());
        }
        let flags = if self.settings.enabled {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if !self.settings.enabled {
            // The last admitted recording has drained. Reject all new writes
            // even if replacing the connection fails and the RW handle must
            // stay alive for a retry.
            self.effective_enabled = false;
            // Readable history must not present a prior process's unfinished
            // rows as live. This bounded recovery writes only end markers;
            // cap trimming and retention remain paused until re-enable.
            close_interrupted_sessions(&mut self.connection, now_unix)?;
            #[cfg(test)]
            if std::mem::take(&mut self.fail_next_read_only_open) {
                return Err(HistoryError::Database);
            }
        }
        let replacement = Connection::open_with_flags(&self.path, flags)?;
        replacement.busy_timeout(Duration::from_secs(5))?;
        replacement.pragma_update(None, "foreign_keys", "ON")?;
        if self.settings.enabled {
            let mut replacement = replacement;
            recover_existing_rows(&mut replacement, now_unix)?;
            self.connection = replacement;
            self.recovered = true;
        } else {
            self.connection = replacement;
        }
        // The old connection is dropped after replacement succeeds. Each write
        // transaction has committed, so SQLite can checkpoint its WAL safely.
        self.writer_open = self.settings.enabled;
        self.effective_enabled = self.settings.enabled;
        self.applied_revision = self.desired_revision;
        Ok(())
    }
}

fn recover_existing_rows(connection: &mut Connection, now_unix: i64) -> Result<(), HistoryError> {
    close_interrupted_sessions(connection, now_unix)?;
    trim_existing_rows(connection)
}

fn validate_activity_id(session_id: &str) -> Result<(), HistoryError> {
    validate_text(
        session_id,
        1,
        crate::model::MAX_SESSION_ID_BYTES,
        "session_id",
    )
    .map_err(HistoryError::Invalid)
}

fn close_interrupted_sessions(
    connection: &mut Connection,
    now_unix: i64,
) -> Result<(), HistoryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut statement =
        transaction.prepare("SELECT session_id FROM sessions WHERE ended_at_unix IS NULL")?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for session_id in ids {
        transaction.execute(
            "UPDATE sessions SET ended_at_unix = ?1,
                 duration_seconds = max(?1 - started_at_unix, 0)
                 WHERE session_id = ?2 AND ended_at_unix IS NULL",
            params![now_unix, session_id],
        )?;
        transaction.execute(
            "INSERT INTO events (session_id, recorded_at_unix, event_type, payload)
                 VALUES (?1, ?2, 'stream_interrupted', NULL)",
            params![session_id, now_unix],
        )?;
        trim_events(&transaction, &session_id)?;
    }
    transaction.commit()?;
    Ok(())
}

/// Upgraded databases can contain rows written before per-session caps
/// existed. Trim them before the first management read, even when the
/// product policy has not loaded or recording is disabled.
fn trim_existing_rows(connection: &mut Connection) -> Result<(), HistoryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let overfull_samples = overfull_sessions(&transaction, "samples", MAX_STORED_SAMPLES)?;
    for session_id in overfull_samples {
        trim_samples(&transaction, &session_id)?;
    }
    let overfull_events = overfull_sessions(&transaction, "events", MAX_STORED_EVENTS)?;
    for session_id in overfull_events {
        trim_events(&transaction, &session_id)?;
    }
    transaction.commit()?;
    Ok(())
}

fn bound_detail_response(detail: &mut SessionDetail) -> Result<(), HistoryError> {
    while serde_json::to_vec(&serde_json::json!({"ok": true, "detail": detail}))
        .map_err(|_| HistoryError::Database)?
        .len()
        > MAX_DETAIL_RESPONSE_BYTES
    {
        if !detail.events.is_empty() {
            detail.events.remove(0);
            detail.events_truncated = true;
        } else if !detail.samples.is_empty() {
            detail.samples.remove(0);
            detail.samples_truncated = true;
        } else {
            return Err(HistoryError::Database);
        }
    }
    Ok(())
}

fn bound_session_page(
    mut sessions: Vec<SessionSummary>,
    offset: usize,
    has_more: bool,
) -> Result<SessionPage, HistoryError> {
    let full_length = sessions.len();
    loop {
        let next_offset = if has_more || sessions.len() < full_length {
            Some(
                offset
                    .checked_add(sessions.len())
                    .ok_or(HistoryError::Invalid(
                        "session page offset exceeds integer range",
                    ))?,
            )
        } else {
            None
        };
        let page = SessionPage {
            sessions,
            next_offset,
        };
        if serde_json::to_vec(&serde_json::json!({
            "ok": true,
            "sessions": &page.sessions,
            "next_offset": page.next_offset,
        }))
        .map_err(|_| HistoryError::Database)?
        .len()
            <= MAX_DETAIL_RESPONSE_BYTES
        {
            return Ok(page);
        }
        sessions = page.sessions;
        if sessions.pop().is_none() {
            return Err(HistoryError::Database);
        }
    }
}

fn apply_schema(connection: &mut Connection) -> Result<(), HistoryError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(HistoryError::Database);
    }
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY NOT NULL,
            protocol TEXT NOT NULL,
            client_name TEXT NOT NULL,
            device_name TEXT NOT NULL,
            app_name TEXT NOT NULL,
            width INTEGER NOT NULL,
            height INTEGER NOT NULL,
            target_fps INTEGER NOT NULL,
            target_bitrate_kbps INTEGER NOT NULL,
            codec TEXT NOT NULL,
            started_at_unix INTEGER NOT NULL,
            ended_at_unix INTEGER,
            duration_seconds INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_sessions_end_time
         ON sessions (ended_at_unix, session_id) WHERE ended_at_unix IS NOT NULL;
         CREATE TABLE IF NOT EXISTS samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
            recorded_at_unix INTEGER NOT NULL,
            frames_sent INTEGER NOT NULL,
            bytes_sent INTEGER NOT NULL,
            dropped_frames INTEGER NOT NULL,
            actual_fps REAL NOT NULL,
            actual_bitrate_kbps REAL NOT NULL,
            encode_latency_ms REAL
         );
         CREATE INDEX IF NOT EXISTS idx_samples_session_time
         ON samples (session_id, recorded_at_unix, id);
         CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
            recorded_at_unix INTEGER NOT NULL,
            event_type TEXT NOT NULL,
            payload TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_events_session_time
         ON events (session_id, recorded_at_unix, id);",
    )?;
    migrate_nullable_encode_latency(connection)?;
    connection.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_samples_session_id ON samples (session_id, id);
         CREATE INDEX IF NOT EXISTS idx_events_session_id ON events (session_id, id);",
    )?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn migrate_nullable_encode_latency(connection: &mut Connection) -> Result<(), HistoryError> {
    let latency_is_required = connection.query_row(
        "SELECT \"notnull\" FROM pragma_table_info('samples') WHERE name = 'encode_latency_ms'",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !latency_is_required {
        return Ok(());
    }

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "DROP INDEX IF EXISTS idx_samples_session_time;
         ALTER TABLE samples RENAME TO samples_before_nullable_latency;
         CREATE TABLE samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
            recorded_at_unix INTEGER NOT NULL,
            frames_sent INTEGER NOT NULL,
            bytes_sent INTEGER NOT NULL,
            dropped_frames INTEGER NOT NULL,
            actual_fps REAL NOT NULL,
            actual_bitrate_kbps REAL NOT NULL,
            encode_latency_ms REAL
         );
         INSERT INTO samples (
            id, session_id, recorded_at_unix, frames_sent, bytes_sent, dropped_frames,
            actual_fps, actual_bitrate_kbps, encode_latency_ms
         )
         SELECT id, session_id, recorded_at_unix, frames_sent, bytes_sent, dropped_frames,
                actual_fps, actual_bitrate_kbps,
                CASE WHEN encode_latency_ms = 0.0 THEN NULL ELSE encode_latency_ms END
         FROM samples_before_nullable_latency;
         DROP TABLE samples_before_nullable_latency;
         CREATE INDEX idx_samples_session_time
         ON samples (session_id, recorded_at_unix, id);",
    )?;
    transaction.commit()?;
    Ok(())
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, HistoryError> {
    Ok(connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
        [table],
        |row| row.get(0),
    )?)
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummary> {
    let start = row.get::<_, i64>(10)?;
    let end = row.get::<_, Option<i64>>(11)?;
    Ok(SessionSummary {
        session_id: row.get(0)?,
        protocol: row.get(1)?,
        client_name: row.get(2)?,
        device_name: row.get(3)?,
        app_name: row.get(4)?,
        width: u32::try_from(row.get::<_, i64>(5)?).unwrap_or_default(),
        height: u32::try_from(row.get::<_, i64>(6)?).unwrap_or_default(),
        target_fps: u32::try_from(row.get::<_, i64>(7)?).unwrap_or_default(),
        target_bitrate_kbps: u32::try_from(row.get::<_, i64>(8)?).unwrap_or_default(),
        codec: row.get(9)?,
        started_at_unix: start,
        ended_at_unix: end,
        duration_seconds: row.get(12)?,
    })
}

fn read_summary(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<SessionSummary>, HistoryError> {
    Ok(connection
        .query_row(
            "SELECT session_id, protocol, client_name, device_name, app_name, width, height,
                    target_fps, target_bitrate_kbps, codec, started_at_unix, ended_at_unix,
                    duration_seconds
             FROM sessions WHERE session_id = ?1",
            [session_id],
            summary_from_row,
        )
        .optional()?)
}

fn read_samples(
    connection: &Connection,
    session_id: &str,
) -> Result<Vec<SessionSample>, HistoryError> {
    // Row IDs preserve admission order when producer timestamps arrive late.
    let mut statement = connection.prepare(
        "SELECT session_id, recorded_at_unix, frames_sent, bytes_sent, dropped_frames,
                actual_fps, actual_bitrate_kbps, encode_latency_ms
         FROM (
            SELECT * FROM samples WHERE session_id = ?1
            ORDER BY id DESC LIMIT ?2
         ) ORDER BY id ASC",
    )?;
    let rows = statement.query_map(params![session_id, MAX_DETAIL_SAMPLES as i64], |row| {
        Ok(SessionSample {
            session_id: row.get(0)?,
            recorded_at_unix: row.get(1)?,
            frames_sent: u64::try_from(row.get::<_, i64>(2)?).unwrap_or_default(),
            bytes_sent: u64::try_from(row.get::<_, i64>(3)?).unwrap_or_default(),
            dropped_frames: u64::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
            actual_fps: row.get(5)?,
            actual_bitrate_kbps: row.get(6)?,
            encode_latency_ms: row.get(7)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn read_events(
    connection: &Connection,
    session_id: &str,
) -> Result<Vec<SessionEvent>, HistoryError> {
    let mut statement = connection.prepare(
        "SELECT session_id, recorded_at_unix, event_type, payload
         FROM (
            SELECT * FROM events WHERE session_id = ?1
            ORDER BY id DESC LIMIT ?2
         ) ORDER BY id ASC",
    )?;
    let rows = statement.query_map(params![session_id, MAX_DETAIL_EVENTS as i64], |row| {
        let payload = row
            .get::<_, Option<String>>(3)?
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
        Ok(SessionEvent {
            session_id: row.get(0)?,
            recorded_at_unix: row.get(1)?,
            event_type: row.get(2)?,
            payload,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn child_count(
    connection: &Connection,
    table: &str,
    session_id: &str,
) -> Result<usize, HistoryError> {
    let sql = match table {
        "samples" => "SELECT count(*) FROM samples WHERE session_id = ?1",
        "events" => "SELECT count(*) FROM events WHERE session_id = ?1",
        _ => return Err(HistoryError::Invalid("invalid history table")),
    };
    let count: i64 = connection.query_row(sql, [session_id], |row| row.get(0))?;
    Ok(usize::try_from(count).unwrap_or_default())
}

fn table_count(connection: &Connection, table: &str) -> Result<usize, HistoryError> {
    let sql = match table {
        "sessions" => "SELECT count(*) FROM sessions",
        "samples" => "SELECT count(*) FROM samples",
        "events" => "SELECT count(*) FROM events",
        _ => return Err(HistoryError::Invalid("invalid history table")),
    };
    let count: i64 = connection.query_row(sql, [], |row| row.get(0))?;
    Ok(usize::try_from(count).unwrap_or_default())
}

fn trim_samples(connection: &Connection, session_id: &str) -> Result<(), HistoryError> {
    connection.execute(
        "DELETE FROM samples WHERE id IN (
            SELECT id FROM samples WHERE session_id = ?1
            ORDER BY id DESC LIMIT -1 OFFSET ?2
        )",
        params![session_id, MAX_STORED_SAMPLES as i64],
    )?;
    Ok(())
}

fn trim_events(connection: &Connection, session_id: &str) -> Result<(), HistoryError> {
    connection.execute(
        "DELETE FROM events WHERE id IN (
            SELECT id FROM events WHERE session_id = ?1
            ORDER BY id DESC LIMIT -1 OFFSET ?2
        )",
        params![session_id, MAX_STORED_EVENTS as i64],
    )?;
    Ok(())
}

fn overfull_sessions(
    connection: &Connection,
    table: &str,
    limit: usize,
) -> Result<Vec<String>, HistoryError> {
    let sql = match table {
        "samples" => "SELECT session_id FROM samples GROUP BY session_id HAVING count(*) > ?1",
        "events" => "SELECT session_id FROM events GROUP BY session_id HAVING count(*) > ?1",
        _ => return Err(HistoryError::Invalid("invalid history table")),
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([limit as i64], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn estimate_live_bytes(connection: &Connection) -> Result<u64, HistoryError> {
    let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let freelist_count: i64 =
        connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    let live_pages = page_count.saturating_sub(freelist_count).max(0) as u64;
    Ok(live_pages.saturating_mul(page_size.max(0) as u64))
}

fn is_active_record(connection: &Connection, session_id: &str) -> Result<bool, HistoryError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1 AND ended_at_unix IS NULL)",
        [session_id],
        |row| row.get(0),
    )?)
}

fn session_exists(connection: &Connection, session_id: &str) -> Result<bool, HistoryError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE session_id = ?1)",
        [session_id],
        |row| row.get(0),
    )?)
}

fn empty_prune_report() -> PruneReport {
    PruneReport {
        sessions_deleted: 0,
        live_bytes: 0,
        size_limit_bytes: 0,
        size_limit_unmet: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;
    use std::path::PathBuf;

    struct TemporaryDatabase(PathBuf);

    impl TemporaryDatabase {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "amaterasu-session-history-{}-{}",
                std::process::id(),
                amaterasu_storage::random_identifier().unwrap()
            )))
        }

        fn path(&self) -> PathBuf {
            self.0.join("history.db")
        }
    }

    impl Drop for TemporaryDatabase {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn nullable_latency_migration_preserves_samples_and_marks_old_zero_as_unknown() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE sessions (
                        session_id TEXT PRIMARY KEY NOT NULL,
                        protocol TEXT NOT NULL,
                        client_name TEXT NOT NULL,
                        device_name TEXT NOT NULL,
                        app_name TEXT NOT NULL,
                        width INTEGER NOT NULL,
                        height INTEGER NOT NULL,
                        target_fps INTEGER NOT NULL,
                        target_bitrate_kbps INTEGER NOT NULL,
                        codec TEXT NOT NULL,
                        started_at_unix INTEGER NOT NULL,
                        ended_at_unix INTEGER,
                        duration_seconds INTEGER
                    );
                    INSERT INTO sessions VALUES
                        ('legacy', 'rtsp', '', '', '', 1, 1, 60, 1000, 'h264', 1, NULL, NULL);
                    CREATE TABLE samples (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
                        recorded_at_unix INTEGER NOT NULL,
                        frames_sent INTEGER NOT NULL,
                        bytes_sent INTEGER NOT NULL,
                        dropped_frames INTEGER NOT NULL,
                        actual_fps REAL NOT NULL,
                        actual_bitrate_kbps REAL NOT NULL,
                        encode_latency_ms REAL NOT NULL
                    );
                    CREATE INDEX idx_samples_session_time
                        ON samples (session_id, recorded_at_unix, id);
                    INSERT INTO samples
                        (session_id, recorded_at_unix, frames_sent, bytes_sent, dropped_frames,
                         actual_fps, actual_bitrate_kbps, encode_latency_ms)
                    VALUES ('legacy', 2, 60, 1200, 1, 60.0, 1200.0, 0.0),
                           ('legacy', 3, 120, 2400, 2, 60.0, 1200.0, 4.25);
                    PRAGMA user_version = 2;",
                )
                .unwrap();
        }

        let store = HistoryStore::open(&path, 10).unwrap();
        let samples = read_samples(&store.connection, "legacy").unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].encode_latency_ms, None);
        assert_eq!(samples[0].frames_sent, 60);
        assert_eq!(samples[1].encode_latency_ms, Some(4.25));
        assert_eq!(samples[1].frames_sent, 120);
        let latency_nullable = store
            .connection
            .query_row(
                "SELECT \"notnull\" FROM pragma_table_info('samples')
                 WHERE name = 'encode_latency_ms'",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap();
        assert!(!latency_nullable);
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION
        );
    }

    fn metadata(session_id: &str) -> SessionMetadata {
        SessionMetadata {
            session_id: session_id.into(),
            protocol: "rtsp".into(),
            client_name: "Client".into(),
            device_name: "Device".into(),
            app_name: "Game".into(),
            width: 1920,
            height: 1080,
            target_fps: 60,
            target_bitrate_kbps: 20_000,
            codec: "h264".into(),
        }
    }

    fn event(session_id: &str, recorded_at_unix: i64, payload: Value) -> SessionEvent {
        SessionEvent {
            session_id: session_id.into(),
            recorded_at_unix,
            event_type: "marker".into(),
            payload: Some(payload),
        }
    }

    fn configure(store: &mut HistoryStore, settings: HistorySettings, now_unix: i64) {
        store.apply_configuration(settings, now_unix).unwrap();
    }

    #[test]
    fn timeline_pages_pin_admission_order_and_detect_pruned_rows() {
        use crate::model::{TimelineCursor, TimelineKind};
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 1).unwrap();
        configure(&mut store, HistorySettings::default(), 1);
        store.begin_session_at(&metadata("timeline"), 1, 1).unwrap();
        store.begin_session_at(&metadata("other"), 1, 1).unwrap();
        for index in 0..70 {
            store
                .record_sample(&SessionSample {
                    session_id: "timeline".into(),
                    recorded_at_unix: 100 - index,
                    frames_sent: index as u64,
                    bytes_sent: index as u64 * 10,
                    dropped_frames: 0,
                    actual_fps: 60.0,
                    actual_bitrate_kbps: 1000.0,
                    encode_latency_ms: None,
                })
                .unwrap();
            if index % 10 == 0 {
                store
                    .record_sample(&SessionSample {
                        session_id: "other".into(),
                        recorded_at_unix: 100,
                        frames_sent: 1,
                        bytes_sent: 10,
                        dropped_frames: 0,
                        actual_fps: 1.0,
                        actual_bitrate_kbps: 1.0,
                        encode_latency_ms: None,
                    })
                    .unwrap();
            }
        }
        for index in 0..12 {
            store
                .record_event(&event(
                    "timeline",
                    index,
                    json!({"index":index,"data":"x".repeat(4000)}),
                ))
                .unwrap();
        }
        let opened = store.timeline_open("timeline", None).unwrap();
        assert_eq!(opened["samples"]["total"], 70);
        assert_eq!(opened["events"]["total"], 13);
        let recent = store.timeline_open("timeline", Some(7)).unwrap();
        assert_eq!(recent["samples"]["total"], 7);
        assert_eq!(recent["events"]["total"], 13);
        assert_eq!(
            store.timeline_open("timeline", Some(2000)).unwrap()["samples"]["total"],
            70
        );
        let recent_cursor: TimelineCursor =
            serde_json::from_value(recent["samples"]["cursor"].clone()).unwrap();
        let recent_page = store
            .timeline_page("timeline", TimelineKind::Samples, recent_cursor, None)
            .unwrap();
        assert_eq!(recent_page["entries"].as_array().unwrap().len(), 7);
        let ids = recent_page["entry_ids"].as_array().unwrap();
        assert_eq!(ids.len(), 7);
        assert!(
            ids.windows(2)
                .all(|pair| pair[0].as_str().unwrap().parse::<i64>().unwrap()
                    < pair[1].as_str().unwrap().parse::<i64>().unwrap())
        );
        assert_eq!(recent_page["entries"][0]["recorded_at_unix"], 37);
        assert_eq!(recent_page["entries"][6]["recorded_at_unix"], 31);
        assert!(recent_page["next_cursor"].is_null());
        assert!(matches!(
            store.timeline_open("timeline", Some(0)),
            Err(HistoryError::Invalid(_))
        ));
        assert!(matches!(
            store.timeline_open("timeline", Some(2001)),
            Err(HistoryError::Invalid(_))
        ));
        let mut sample_cursor: TimelineCursor =
            serde_json::from_value(opened["samples"]["cursor"].clone()).unwrap();
        let first = store
            .timeline_page(
                "timeline",
                TimelineKind::Samples,
                sample_cursor.clone(),
                Some(32),
            )
            .unwrap();
        assert_eq!(first["entries"].as_array().unwrap().len(), 32);
        assert_eq!(first["entries"][0]["recorded_at_unix"], 100);
        sample_cursor = serde_json::from_value(first["next_cursor"].clone()).unwrap();
        // Later appends remain outside this snapshot, including for an active stream.
        store
            .record_sample(&SessionSample {
                session_id: "timeline".into(),
                recorded_at_unix: 200,
                frames_sent: 70,
                bytes_sent: 700,
                dropped_frames: 0,
                actual_fps: 60.0,
                actual_bitrate_kbps: 1000.0,
                encode_latency_ms: None,
            })
            .unwrap();
        let mut samples = 32;
        loop {
            let page = store
                .timeline_page("timeline", TimelineKind::Samples, sample_cursor, Some(32))
                .unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_DETAIL_RESPONSE_BYTES);
            samples += page["entries"].as_array().unwrap().len();
            if page["next_cursor"].is_null() {
                break;
            }
            sample_cursor = serde_json::from_value(page["next_cursor"].clone()).unwrap();
        }
        assert_eq!(samples, 70);
        let mut cursor: TimelineCursor =
            serde_json::from_value(opened["events"]["cursor"].clone()).unwrap();
        let mut events = 0;
        loop {
            let page = store
                .timeline_page("timeline", TimelineKind::Events, cursor, Some(8))
                .unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_DETAIL_RESPONSE_BYTES);
            events += page["entries"].as_array().unwrap().len();
            if page["next_cursor"].is_null() {
                break;
            }
            cursor = serde_json::from_value(page["next_cursor"].clone()).unwrap();
        }
        assert_eq!(events, 13);
        let cursor: TimelineCursor =
            serde_json::from_value(opened["samples"]["cursor"].clone()).unwrap();
        let foreign_id: i64 = store
            .connection
            .query_row(
                "SELECT min(id) FROM samples WHERE session_id = 'other'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut foreign_cursor = cursor.clone();
        foreign_cursor.after_id = Some(foreign_id.to_string());
        assert!(matches!(
            store.timeline_page("timeline", TimelineKind::Samples, foreign_cursor, None),
            Err(HistoryError::Invalid(_))
        ));
        let mut wrong_session = cursor.clone();
        wrong_session.session_id = "other".to_owned();
        assert!(matches!(
            store.timeline_page("timeline", TimelineKind::Samples, wrong_session, None),
            Err(HistoryError::Invalid(_))
        ));
        let mut wrong_kind = cursor.clone();
        wrong_kind.kind = TimelineKind::Events;
        assert!(matches!(
            store.timeline_page("timeline", TimelineKind::Samples, wrong_kind, None),
            Err(HistoryError::Invalid(_))
        ));
        store
            .connection
            .execute(
                "DELETE FROM samples WHERE session_id = ?1 AND id = ?2",
                params!["timeline", cursor.first_id.parse::<i64>().unwrap()],
            )
            .unwrap();
        assert!(matches!(
            store.timeline_page("timeline", TimelineKind::Samples, cursor, None),
            Err(HistoryError::HistoryChanged)
        ));
    }

    #[test]
    fn pending_launch_holds_recording_policy_until_attached_stream_ends() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        assert!(store.begin_stream_activity("launch", 101).unwrap());
        assert_eq!(store.status().unwrap().pending_activities, 1);
        assert_eq!(store.status().unwrap().active_streams, 0);
        assert!(store.begin_stream_activity("launch", 200).unwrap());
        assert_eq!(store.status().unwrap().pending_activities, 1);
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            201,
        );
        assert!(store.settings().1);
        assert!(
            store
                .begin_session_at(&metadata("launch"), 210, 210)
                .unwrap()
        );
        assert_eq!(store.status().unwrap().pending_activities, 0);
        assert_eq!(store.status().unwrap().active_streams, 1);
        assert_eq!(store.status().unwrap().events, 1);
        assert!(store.end_session("launch", 220, 220).unwrap());
        assert!(!store.settings().1);
        assert_eq!(store.status().unwrap().events, 2);
        assert!(!store.end_stream_activity("launch", 221).unwrap());
        assert!(!store.end_stream_activity("launch", 222).unwrap());
        assert!(store.begin_stream_activity("launch", 223).is_err());
    }

    #[test]
    fn cancelled_and_expired_launches_release_policy_without_late_recording() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        assert!(store.begin_stream_activity("cancelled", 101).unwrap());
        assert!(store.begin_stream_activity("expired", 102).unwrap());
        assert_eq!(store.status().unwrap().pending_activities, 2);
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            103,
        );
        assert!(store.end_stream_activity("cancelled", 104).unwrap());
        assert_eq!(store.status().unwrap().pending_activities, 1);
        assert!(store.settings().1);
        // A retry cannot extend the first deadline of 402.
        assert!(store.begin_stream_activity("expired", 401).unwrap());
        store.prune_if_due(402);
        assert_eq!(store.status().unwrap().pending_activities, 0);
        assert!(!store.settings().1);
        assert!(store.begin_stream_activity("expired", 403).is_err());
        assert!(
            store
                .begin_session_at(&metadata("expired"), 403, 403)
                .is_err()
        );
        assert!(!store.end_stream_activity("expired", 404).unwrap());
        assert_eq!(store.status().unwrap().sessions, 0);
    }

    #[test]
    fn new_launch_after_expiry_uses_saved_disabled_policy() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        assert!(store.begin_stream_activity("expired", 101).unwrap());
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            102,
        );
        assert!(store.settings().1);

        // There is no status/prune poll between expiry and the next admission.
        assert!(store.begin_stream_activity("new", 401).unwrap());
        assert!(!store.settings().1);
        assert_eq!(store.status().unwrap().pending_activities, 1);
        assert!(store.begin_stream_activity("expired", 402).is_err());
        assert!(!store.begin_session_at(&metadata("new"), 403, 403).unwrap());
        assert!(!store.end_session("new", 404, 404).unwrap());
        assert_eq!(store.status().unwrap().sessions, 0);
    }

    #[test]
    fn clock_rollback_cannot_extend_pending_toggle_deferral() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 1000).unwrap();
        configure(&mut store, HistorySettings::default(), 1000);
        assert!(store.begin_stream_activity("pending", 1000).unwrap());
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            1001,
        );

        // A wall-clock rollback alone must not expire a fresh reservation.
        store.prune_if_due(-2600);
        assert!(store.settings().1);
        assert_eq!(store.status().unwrap().pending_activities, 1);

        // Model five real minutes passing while Unix time remains rolled back.
        store
            .pending_activities
            .get_mut("pending")
            .unwrap()
            .reserved_at = Instant::now()
            .checked_sub(Duration::from_secs(ACTIVITY_RESERVATION_SECONDS as u64 + 1))
            .unwrap();
        store.prune_if_due(-2599);
        assert!(!store.settings().1);
        assert_eq!(store.status().unwrap().pending_activities, 0);
        assert!(store.begin_stream_activity("pending", -2598).is_err());
    }

    #[test]
    fn direct_session_after_expiry_uses_saved_enabled_policy() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            100,
        );
        assert!(store.begin_stream_activity("expired", 101).unwrap());
        configure(&mut store, HistorySettings::default(), 102);
        assert!(!store.settings().1);

        assert!(store.begin_session_at(&metadata("new"), 403, 401).unwrap());
        assert!(store.settings().1);
        assert!(store.begin_stream_activity("expired", 402).is_err());
        assert!(store.end_session("new", 404, 404).unwrap());
        assert_eq!(store.status().unwrap().sessions, 1);
    }

    #[test]
    fn disabled_launch_holds_disabled_policy_and_restart_clears_reservations() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        let mut store = HistoryStore::open(&path, 100).unwrap();
        assert!(!store.begin_stream_activity("not-ready", 100).unwrap());
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            101,
        );
        assert!(store.begin_stream_activity("launch", 102).unwrap());
        assert_eq!(store.status().unwrap().pending_activities, 1);
        configure(&mut store, HistorySettings::default(), 103);
        assert!(!store.settings().1);
        assert!(
            !store
                .begin_session_at(&metadata("launch"), 104, 104)
                .unwrap()
        );
        assert!(!store.end_session("launch", 105, 105).unwrap());
        assert!(store.settings().1);
        assert!(!store.end_stream_activity("launch", 106).unwrap());
        assert!(store.begin_stream_activity("launch", 106).is_err());
        drop(store);
        let mut reopened = HistoryStore::open(&path, 107).unwrap();
        configure(&mut reopened, HistorySettings::default(), 107);
        assert_eq!(reopened.status().unwrap().pending_activities, 0);
        assert!(reopened.begin_stream_activity("launch", 108).unwrap());
    }

    #[test]
    fn end_before_begin_and_terminal_capacity_are_bounded() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            100,
        );
        assert!(store.end_stream_activity("cancelled", 101).unwrap());
        assert!(!store.end_stream_activity("cancelled", 102).unwrap());
        assert!(store.begin_stream_activity("cancelled", 103).is_err());
        assert!(!store.end_session("early-end", 103, 103).unwrap());
        assert!(store.begin_stream_activity("early-end", 104).is_err());
        assert!(
            store
                .begin_session_at(&metadata("early-end"), 104, 104)
                .is_err()
        );
        for index in 0..=MAX_ACTIVITY_IDS {
            store
                .end_stream_activity(&format!("unused-{index}"), 104)
                .unwrap();
        }
        assert_eq!(store.terminal_activities.len(), MAX_ACTIVITY_IDS);
        assert_eq!(store.terminal_order.len(), MAX_ACTIVITY_IDS);
    }

    #[test]
    fn attached_stream_does_not_expire_with_its_launch_reservation() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        assert!(store.begin_stream_activity("attached", 101).unwrap());
        assert!(
            store
                .begin_session_at(&metadata("attached"), 102, 102)
                .unwrap()
        );
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            103,
        );
        store.prune_if_due(1000);
        assert!(store.settings().1);
        assert!(store.end_session("attached", 1001, 1001).unwrap());
        assert!(!store.settings().1);
    }

    #[test]
    fn defaults_and_nonnegative_ranges_match_the_global_contract() {
        assert_eq!(
            HistorySettings::default(),
            HistorySettings {
                enabled: true,
                ttl_days: 0,
                db_size_limit_mb: 0,
            }
        );
        assert!(
            HistorySettings {
                enabled: false,
                ttl_days: i32::MAX,
                db_size_limit_mb: i32::MAX,
            }
            .validate()
            .is_ok()
        );
        assert!(
            HistorySettings {
                ttl_days: -1,
                ..HistorySettings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            HistorySettings {
                db_size_limit_mb: -1,
                ..HistorySettings::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn session_samples_events_survive_reopen_with_central_policy() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        {
            let mut store = HistoryStore::open(&path, 100).unwrap();
            configure(&mut store, HistorySettings::default(), 100);
            assert!(store.begin_session(&metadata("session-1"), 100).unwrap());
            assert!(
                store
                    .record_sample(&SessionSample {
                        session_id: "session-1".into(),
                        recorded_at_unix: 105,
                        frames_sent: 60,
                        bytes_sent: 10_000,
                        dropped_frames: 2,
                        actual_fps: 58.5,
                        actual_bitrate_kbps: 18_000.0,
                        encode_latency_ms: Some(4.25),
                    })
                    .unwrap()
            );
            assert!(
                store
                    .record_event(&event("session-1", 106, json!({"marker":"test"})))
                    .unwrap()
            );
            assert!(store.end_session("session-1", 110, 110).unwrap());
        }

        let mut store = HistoryStore::open(&path, 120).unwrap();
        assert!(!store.status().unwrap().configuration_ready);
        configure(
            &mut store,
            HistorySettings {
                enabled: true,
                ttl_days: 12,
                db_size_limit_mb: 8,
            },
            120,
        );
        let (settings, effective_enabled) = store.settings();
        assert_eq!(settings.ttl_days, 12);
        assert_eq!(settings.db_size_limit_mb, 8);
        assert!(effective_enabled);
        assert_eq!(store.list_sessions(25, 0).unwrap().len(), 1);
        let detail = store.session_detail("session-1").unwrap();
        assert_eq!(detail.session.duration_seconds, Some(10));
        assert_eq!(detail.sample_count, 1);
        assert_eq!(detail.event_count, 3);
        assert_eq!(detail.samples[0].frames_sent, 60);
        assert_eq!(detail.events[1].event_type, "marker");
    }

    #[test]
    fn v1_settings_remain_available_until_the_central_document_is_confirmed() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        amaterasu_storage::create_private_directory(path.parent().unwrap()).unwrap();
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE history_settings (
                        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                        enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
                        ttl_days INTEGER NOT NULL CHECK (ttl_days >= 0),
                        db_size_limit_mb INTEGER NOT NULL CHECK (db_size_limit_mb >= 0),
                        revision INTEGER NOT NULL CHECK (revision >= 1)
                     );
                     INSERT INTO history_settings
                     (singleton, enabled, ttl_days, db_size_limit_mb, revision)
                     VALUES (1, 0, 9, 17, 4);
                     PRAGMA user_version = 1;",
                )
                .unwrap();
        }

        let mut store = HistoryStore::open(&path, 200).unwrap();
        assert_eq!(
            store.legacy_settings().unwrap(),
            Some(HistorySettings {
                enabled: false,
                ttl_days: 9,
                db_size_limit_mb: 17,
            })
        );
        assert!(table_exists(&store.connection, "history_settings").unwrap());

        let central = HistorySettings {
            enabled: false,
            ttl_days: 9,
            db_size_limit_mb: 17,
        };
        store.apply_configuration(central, 200).unwrap();
        assert!(!store.discard_legacy_settings().unwrap());
        assert!(table_exists(&store.connection, "history_settings").unwrap());
        store
            .apply_configuration(HistorySettings::default(), 201)
            .unwrap();
        assert!(store.discard_legacy_settings().unwrap());
        assert!(!table_exists(&store.connection, "history_settings").unwrap());
        assert!(store.status().unwrap().configuration_ready);
    }

    #[test]
    fn streaming_before_configuration_load_does_not_create_a_history_record() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();

        assert!(
            !store
                .begin_session(&metadata("config-pending"), 100)
                .unwrap()
        );
        let pending = store.status().unwrap();
        assert!(!pending.configuration_ready);
        assert!(!pending.effective_enabled);
        assert!(pending.degraded);
        assert_eq!(pending.sessions, 0);

        store
            .apply_configuration(HistorySettings::default(), 101)
            .unwrap();
        assert!(!store.status().unwrap().effective_enabled);
        assert!(!store.end_session("config-pending", 110, 110).unwrap());
        assert!(store.status().unwrap().effective_enabled);
        assert_eq!(store.list_sessions(25, 0).unwrap().len(), 0);
    }

    #[test]
    fn reopening_marks_unclosed_sessions_as_interrupted() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        {
            let mut store = HistoryStore::open(&path, 100).unwrap();
            configure(&mut store, HistorySettings::default(), 100);
            store.begin_session(&metadata("interrupted"), 90).unwrap();
        }

        let mut store = HistoryStore::open(&path, 150).unwrap();
        configure(&mut store, HistorySettings::default(), 150);
        let detail = store.session_detail("interrupted").unwrap();
        assert_eq!(detail.session.ended_at_unix, Some(150));
        assert_eq!(detail.session.duration_seconds, Some(60));
        assert!(
            detail
                .events
                .iter()
                .any(|event| event.event_type == "stream_interrupted")
        );
        assert!(store.active_sessions(16).unwrap().is_empty());
    }

    #[test]
    fn failed_begin_rolls_back_and_does_not_leave_an_active_stream() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_stream_started BEFORE INSERT ON events
                 WHEN NEW.event_type = 'stream_started'
                 BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();

        assert!(matches!(
            store.begin_session(&metadata("failed"), 100),
            Err(HistoryError::Database)
        ));
        assert!(store.list_sessions(25, 0).unwrap().is_empty());
        assert_eq!(store.status().unwrap().active_streams, 0);
    }

    #[test]
    fn failed_end_keeps_the_active_stream_tracked_for_retry() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        store.begin_session(&metadata("retry-end"), 100).unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_stream_ended BEFORE INSERT ON events
                 WHEN NEW.event_type = 'stream_ended'
                 BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();

        assert!(matches!(
            store.end_session("retry-end", 110, 110),
            Err(HistoryError::Database)
        ));
        assert_eq!(store.status().unwrap().active_streams, 1);
        assert_eq!(store.active_sessions(16).unwrap().len(), 1);
        store
            .connection
            .execute_batch("DROP TRIGGER reject_stream_ended")
            .unwrap();
        assert!(store.end_session("retry-end", 110, 110).unwrap());
        assert_eq!(store.status().unwrap().active_streams, 0);
    }

    #[test]
    fn failed_sample_and_event_writes_retry_in_admission_order_after_reopen() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        let sample = |recorded_at_unix| SessionSample {
            session_id: "retry-record".into(),
            recorded_at_unix,
            frames_sent: recorded_at_unix as u64,
            bytes_sent: 100,
            dropped_frames: 0,
            actual_fps: 60.0,
            actual_bitrate_kbps: 1_000.0,
            encode_latency_ms: Some(1.0),
        };
        {
            let mut store = HistoryStore::open(&path, 100).unwrap();
            configure(&mut store, HistorySettings::default(), 100);
            store.begin_session(&metadata("retry-record"), 100).unwrap();
            store.record_sample(&sample(110)).unwrap();
            store
                .record_event(&event("retry-record", 110, json!("first")))
                .unwrap();
            store
                .connection
                .execute_batch(
                    "CREATE TRIGGER reject_sample BEFORE INSERT ON samples
                     BEGIN SELECT RAISE(ABORT, 'test failure'); END;
                     CREATE TRIGGER reject_event BEFORE INSERT ON events
                     WHEN NEW.event_type = 'marker'
                     BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
                )
                .unwrap();
            assert!(matches!(
                store.record_sample(&sample(105)),
                Err(HistoryError::Database)
            ));
            assert!(matches!(
                store.record_event(&event("retry-record", 105, json!("retried"))),
                Err(HistoryError::Database)
            ));
            let detail = store.session_detail("retry-record").unwrap();
            assert_eq!(detail.sample_count, 1);
            assert_eq!(detail.event_count, 2);
            store
                .connection
                .execute_batch("DROP TRIGGER reject_sample; DROP TRIGGER reject_event;")
                .unwrap();
            assert!(store.record_sample(&sample(105)).unwrap());
            assert!(
                store
                    .record_event(&event("retry-record", 105, json!("retried")))
                    .unwrap()
            );
            store.end_session("retry-record", 120, 120).unwrap();
        }

        let store = HistoryStore::open(&path, 130).unwrap();
        let detail = store.session_detail("retry-record").unwrap();
        assert_eq!(detail.sample_count, 2);
        assert_eq!(detail.event_count, 4);
        assert_eq!(
            detail
                .samples
                .iter()
                .map(|sample| sample.recorded_at_unix)
                .collect::<Vec<_>>(),
            [110, 105]
        );
        assert_eq!(
            detail
                .events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            ["stream_started", "marker", "marker", "stream_ended"]
        );
        assert_eq!(detail.events[2].payload, Some(json!("retried")));
    }

    #[test]
    fn enabled_changes_wait_until_the_last_active_session_ends() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            100,
        );
        assert!(!store.begin_session(&metadata("unrecorded"), 110).unwrap());
        configure(&mut store, HistorySettings::default(), 120);
        assert!(!store.settings().1);
        assert!(
            !store
                .begin_session(&metadata("also-unrecorded"), 121)
                .unwrap()
        );
        assert!(!store.end_session("unrecorded", 130, 130).unwrap());
        assert!(!store.settings().1);
        assert!(!store.end_session("also-unrecorded", 131, 131).unwrap());
        assert!(store.settings().1);
        assert!(
            store
                .begin_session(&metadata("recorded-after-drain"), 132)
                .unwrap()
        );
        assert!(store.end_session("recorded-after-drain", 133, 133).unwrap());
        assert_eq!(store.list_sessions(25, 0).unwrap().len(), 1);
    }

    #[test]
    fn age_pruning_removes_expired_ended_sessions_and_preserves_active_sessions() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 200_000).unwrap();
        configure(
            &mut store,
            HistorySettings {
                enabled: true,
                ttl_days: 1,
                db_size_limit_mb: 0,
            },
            200_000,
        );
        store.begin_session(&metadata("expired"), 100).unwrap();
        store.begin_session(&metadata("recent"), 190_000).unwrap();
        store.begin_session(&metadata("active"), 100).unwrap();
        store.end_session("expired", 101, 101).unwrap();
        store.end_session("recent", 190_100, 190_100).unwrap();

        let sessions = store.list_sessions(25, 0).unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(
            sessions
                .iter()
                .any(|session| session.session_id == "recent")
        );
        assert!(
            sessions
                .iter()
                .any(|session| session.session_id == "active")
        );
        assert_eq!(store.active_sessions(16).unwrap().len(), 1);
        assert_eq!(store.active_sessions(16).unwrap()[0].session_id, "active");
    }

    #[test]
    fn disabled_recording_keeps_readable_history_after_restart() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        {
            let mut store = HistoryStore::open(&path, 100).unwrap();
            configure(&mut store, HistorySettings::default(), 100);
            store.begin_session(&metadata("expired"), 100).unwrap();
            store.end_session("expired", 110, 110).unwrap();
            store.begin_session(&metadata("recent"), 170_000).unwrap();
            store.end_session("recent", 170_010, 170_010).unwrap();
        }

        let mut store = HistoryStore::open(&path, 200_000).unwrap();
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ttl_days: 1,
                db_size_limit_mb: 0,
            },
            200_000,
        );
        assert!(!store.settings().1);
        assert!(!store.writer_open);
        assert_eq!(
            store.session_detail("expired").unwrap().session.session_id,
            "expired"
        );
        assert_eq!(
            store.session_detail("recent").unwrap().session.session_id,
            "recent"
        );
        assert_eq!(store.status().unwrap().last_prune.sessions_deleted, 0);
        assert!(
            !store
                .begin_session(&metadata("unrecorded"), 200_001)
                .unwrap()
        );
        assert_eq!(store.status().unwrap().sessions, 2);
    }

    #[test]
    fn disabled_writer_does_not_run_periodic_retention() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        store
            .begin_session(&metadata("expires-later"), 100)
            .unwrap();
        store.end_session("expires-later", 101, 101).unwrap();
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ttl_days: 1,
                db_size_limit_mb: 0,
            },
            200,
        );
        assert_eq!(store.status().unwrap().sessions, 1);
        store.prune_if_due(200 + PRUNE_INTERVAL_SECONDS - 1);
        assert_eq!(store.status().unwrap().sessions, 1);

        // The process stays open and no session ends while wall time advances.
        store.prune_if_due(86_502);
        assert_eq!(store.status().unwrap().sessions, 1);
        assert_eq!(store.status().unwrap().last_prune.sessions_deleted, 0);
        assert!(!store.settings().1);
        assert!(!store.writer_open);
    }

    #[test]
    fn final_recorded_end_prunes_before_writer_closes_then_idle_prune_stops() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        store.begin_session(&metadata("old"), 100).unwrap();
        store.end_session("old", 101, 101).unwrap();
        store.begin_session(&metadata("last"), 200).unwrap();
        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ttl_days: 1,
                db_size_limit_mb: 0,
            },
            201,
        );
        assert!(store.writer_open);
        assert_eq!(store.status().unwrap().sessions, 2);

        store.end_session("last", 86_500, 86_502).unwrap();
        assert!(!store.writer_open);
        assert!(matches!(
            store.session_detail("old"),
            Err(HistoryError::NotFound)
        ));
        assert_eq!(store.status().unwrap().last_prune.sessions_deleted, 1);
        store.prune_if_due(200_000);
        assert_eq!(store.status().unwrap().sessions, 1);
        assert_eq!(store.status().unwrap().last_prune.sessions_deleted, 1);
    }

    #[test]
    fn disabled_recording_does_not_apply_size_limit_to_ended_records() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        store.begin_session(&metadata("large"), 100).unwrap();
        for index in 0..300 {
            store
                .record_event(&event("large", 101 + index, json!("x".repeat(4000))))
                .unwrap();
        }
        store.end_session("large", 500, 500).unwrap();

        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ttl_days: 0,
                db_size_limit_mb: 1,
            },
            600,
        );
        assert!(!store.settings().1);
        assert_eq!(
            store.session_detail("large").unwrap().session.session_id,
            "large"
        );
        let status = store.status().unwrap();
        assert_eq!(status.last_prune.sessions_deleted, 0);
        assert!(!status.last_prune.size_limit_unmet);
    }

    #[test]
    fn disabling_during_a_stream_keeps_its_recording_until_all_streams_end() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        let mut store = HistoryStore::open(&path, 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        assert!(store.begin_session(&metadata("expired"), 100).unwrap());
        assert!(store.end_session("expired", 101, 101).unwrap());
        assert!(store.begin_session(&metadata("first"), 200_000).unwrap());

        configure(
            &mut store,
            HistorySettings {
                enabled: false,
                ttl_days: 1,
                db_size_limit_mb: 0,
            },
            200_001,
        );
        assert!(!store.settings().0.enabled);
        assert!(store.settings().1);
        assert_eq!(store.status().unwrap().last_prune.sessions_deleted, 1);
        assert_eq!(store.active_sessions(16).unwrap().len(), 1);
        assert!(store.begin_session(&metadata("second"), 200_002).unwrap());
        assert!(
            store
                .record_event(&event("first", 200_003, json!("continuing")))
                .unwrap()
        );
        assert!(store.end_session("first", 200_003, 200_003).unwrap());
        assert!(store.settings().1);
        assert!(store.end_session("second", 200_004, 200_004).unwrap());
        assert!(!store.settings().1);
        assert!(!store.begin_session(&metadata("third"), 200_005).unwrap());
        assert!(!store.end_session("third", 200_006, 200_006).unwrap());
        assert_eq!(store.status().unwrap().sessions, 2);

        drop(store);
        let mut reopened = HistoryStore::open(&path, 200_010).unwrap();
        configure(
            &mut reopened,
            HistorySettings {
                enabled: false,
                ttl_days: 1,
                db_size_limit_mb: 0,
            },
            200_010,
        );
        assert_eq!(reopened.list_sessions(25, 0).unwrap().len(), 2);
        assert_eq!(reopened.session_detail("first").unwrap().event_count, 3);
        assert_eq!(reopened.status().unwrap().last_prune.sessions_deleted, 0);
        assert!(reopened.active_sessions(16).unwrap().is_empty());
    }

    #[test]
    fn size_pruning_removes_oldest_ended_sessions_without_deleting_active_records() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(
            &mut store,
            HistorySettings {
                enabled: true,
                ttl_days: 0,
                db_size_limit_mb: 1,
            },
            100,
        );
        store.begin_session(&metadata("oldest"), 10).unwrap();
        for index in 0..150 {
            store
                .record_event(&event("oldest", 11 + index, json!("x".repeat(4000))))
                .unwrap();
        }
        store.end_session("oldest", 200, 200).unwrap();
        store.begin_session(&metadata("newer"), 300).unwrap();
        for index in 0..150 {
            store
                .record_event(&event("newer", 301 + index, json!("y".repeat(4000))))
                .unwrap();
        }
        store.end_session("newer", 500, 500).unwrap();
        store.begin_session(&metadata("still-active"), 600).unwrap();

        let report = store.status().unwrap().last_prune;
        let sessions = store.list_sessions(25, 0).unwrap();
        assert!(report.sessions_deleted >= 1);
        assert!(
            !sessions
                .iter()
                .any(|session| session.session_id == "oldest")
        );
        assert!(
            sessions
                .iter()
                .any(|session| session.session_id == "still-active"
                    && session.ended_at_unix.is_none())
        );
        assert!(
            store
                .active_sessions(16)
                .unwrap()
                .iter()
                .any(|session| session.session_id == "still-active")
        );
    }

    #[test]
    fn active_sessions_cannot_be_deleted_and_child_rows_cascade_for_ended_sessions() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 10).unwrap();
        configure(&mut store, HistorySettings::default(), 10);
        store.begin_session(&metadata("active"), 10).unwrap();
        assert!(matches!(
            store.delete_session("active"),
            Err(HistoryError::ActiveSession)
        ));
        store
            .record_event(&event("active", 11, json!({"persist":true})))
            .unwrap();
        store.end_session("active", 12, 12).unwrap();
        store.delete_session("active").unwrap();
        assert!(matches!(
            store.session_detail("active"),
            Err(HistoryError::NotFound)
        ));
    }

    #[test]
    fn detail_reads_are_bounded_and_report_truncation() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 10).unwrap();
        configure(&mut store, HistorySettings::default(), 10);
        store.begin_session(&metadata("bounded"), 10).unwrap();
        for index in 0..40 {
            store
                .record_sample(&SessionSample {
                    session_id: "bounded".into(),
                    recorded_at_unix: 11 + index,
                    frames_sent: index as u64,
                    bytes_sent: 0,
                    dropped_frames: 0,
                    actual_fps: 60.0,
                    actual_bitrate_kbps: 20_000.0,
                    encode_latency_ms: Some(1.0),
                })
                .unwrap();
        }
        for index in 0..12 {
            store
                .record_event(&event("bounded", 60 + index, json!({"index":index})))
                .unwrap();
        }
        store.end_session("bounded", 80, 80).unwrap();

        let detail = store.session_detail("bounded").unwrap();
        assert_eq!(detail.sample_count, 40);
        assert!(detail.samples_truncated);
        assert_eq!(detail.samples.len(), MAX_DETAIL_SAMPLES);
        assert_eq!(detail.event_count, 14);
        assert!(detail.events_truncated);
        assert_eq!(detail.events.len(), MAX_DETAIL_EVENTS);
        assert!(matches!(
            store.list_sessions(26, 0),
            Err(HistoryError::Invalid(_))
        ));
        assert!(matches!(
            store.active_sessions(17),
            Err(HistoryError::Invalid(_))
        ));
    }

    #[test]
    fn detail_with_near_limit_events_fits_broker_response() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 10).unwrap();
        configure(&mut store, HistorySettings::default(), 10);
        store.begin_session(&metadata("large-detail"), 10).unwrap();
        for index in 0..MAX_DETAIL_SAMPLES {
            store
                .record_sample(&SessionSample {
                    session_id: "large-detail".into(),
                    recorded_at_unix: 11 + index as i64,
                    frames_sent: index as u64,
                    bytes_sent: 123_456,
                    dropped_frames: 0,
                    actual_fps: 60.0,
                    actual_bitrate_kbps: 20_000.0,
                    encode_latency_ms: Some(1.0),
                })
                .unwrap();
        }
        for index in 0..MAX_DETAIL_EVENTS {
            store
                .record_event(&event(
                    "large-detail",
                    60 + index as i64,
                    json!({"index": index, "data": "x".repeat(4_000)}),
                ))
                .unwrap();
        }
        store.end_session("large-detail", 80, 80).unwrap();

        let detail = store.session_detail("large-detail").unwrap();
        let response = json!({"ok":true,"detail":detail});
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_DETAIL_RESPONSE_BYTES);
        assert_eq!(detail.sample_count, MAX_DETAIL_SAMPLES);
        assert_eq!(detail.samples.len(), MAX_DETAIL_SAMPLES);
        assert!(detail.event_count > detail.events.len());
        assert!(detail.events_truncated);
        assert_eq!(detail.events.last().unwrap().event_type, "stream_ended");
        assert!(detail.events.iter().any(|event| {
            event
                .payload
                .as_ref()
                .is_some_and(|payload| payload["index"] == 7)
        }));
    }

    #[test]
    fn maximal_session_summaries_page_without_skips_after_restart() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        let mut store = HistoryStore::open(&path, 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        let mut expected = Vec::new();
        for index in 0..35 {
            let session_id = format!("session-{index:02}{}", "x".repeat(118));
            let mut session = metadata(&session_id);
            session.protocol = "p".repeat(32);
            session.client_name = "c".repeat(128);
            session.device_name = "d".repeat(128);
            session.app_name = "a".repeat(128);
            session.codec = "v".repeat(32);
            session.width = u32::MAX;
            session.height = u32::MAX;
            session.target_fps = u32::MAX;
            session.target_bitrate_kbps = u32::MAX;
            store.begin_session(&session, 100 + index).unwrap();
            expected.push(session_id);
        }
        expected.reverse();

        let mut active_offset = 0;
        let mut active_ids = Vec::new();
        loop {
            let page = store.active_sessions_page(16, active_offset).unwrap();
            assert!(
                serde_json::to_vec(
                    &json!({"ok":true,"sessions":page.sessions,"next_offset":page.next_offset})
                )
                .unwrap()
                .len()
                    <= MAX_DETAIL_RESPONSE_BYTES
            );
            active_ids.extend(
                page.sessions
                    .iter()
                    .map(|session| session.session_id.clone()),
            );
            match page.next_offset {
                Some(next) => {
                    assert_eq!(next, active_offset + page.sessions.len());
                    active_offset = next;
                }
                None => break,
            }
        }
        assert_eq!(active_ids, expected);
        drop(store);

        let mut reopened = HistoryStore::open(&path, 200).unwrap();
        configure(
            &mut reopened,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            200,
        );
        let mut offset = 0;
        let mut saved_ids = Vec::new();
        let mut saw_short_page = false;
        loop {
            let page = reopened.list_sessions_page(25, offset).unwrap();
            assert!(
                serde_json::to_vec(
                    &json!({"ok":true,"sessions":page.sessions,"next_offset":page.next_offset})
                )
                .unwrap()
                .len()
                    <= MAX_DETAIL_RESPONSE_BYTES
            );
            saw_short_page |= page.sessions.len() < 25 && page.next_offset.is_some();
            saved_ids.extend(
                page.sessions
                    .iter()
                    .map(|session| session.session_id.clone()),
            );
            match page.next_offset {
                Some(next) => {
                    assert_eq!(next, offset + page.sessions.len());
                    offset = next;
                }
                None => break,
            }
        }
        assert!(saw_short_page);
        assert_eq!(saved_ids, expected);
        assert!(
            reopened
                .active_sessions_page(16, 0)
                .unwrap()
                .sessions
                .is_empty()
        );
        assert!(
            reopened.list_sessions_page(1, 0).unwrap().sessions[0]
                .ended_at_unix
                .is_some()
        );
    }

    #[test]
    fn exact_full_last_page_has_no_continuation() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        store.begin_session(&metadata("first"), 100).unwrap();
        store.begin_session(&metadata("second"), 101).unwrap();

        let full = store.list_sessions_page(2, 0).unwrap();
        assert_eq!(full.sessions.len(), 2);
        assert_eq!(full.next_offset, None);
        let first = store.active_sessions_page(1, 0).unwrap();
        assert_eq!(first.next_offset, Some(1));
        let last = store.active_sessions_page(1, 1).unwrap();
        assert_eq!(last.sessions.len(), 1);
        assert_eq!(last.next_offset, None);
    }

    #[test]
    fn sample_and_event_storage_keeps_the_latest_admitted_per_session_after_restart() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        let mut store = HistoryStore::open(&path, 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        store
            .begin_session(&metadata("bounded-storage"), 100)
            .unwrap();

        // Seed a full history in one transaction; the public writes at the
        // boundary exercise both retention paths and their readback.
        {
            let transaction = store.connection.transaction().unwrap();
            {
                let mut insert = transaction
                    .prepare(
                        "INSERT INTO samples (session_id, recorded_at_unix, frames_sent,
                     bytes_sent, dropped_frames, actual_fps, actual_bitrate_kbps,
                     encode_latency_ms) VALUES ('bounded-storage', ?1, 0, 0, 0, 0, 0, 0)",
                    )
                    .unwrap();
                for timestamp in 101..=100 + MAX_STORED_SAMPLES as i64 {
                    insert.execute([timestamp]).unwrap();
                }
            }
            {
                let mut insert = transaction
                    .prepare(
                        "INSERT INTO events (session_id, recorded_at_unix, event_type, payload)
                     VALUES ('bounded-storage', ?1, 'marker', NULL)",
                    )
                    .unwrap();
                for timestamp in 101..100 + MAX_STORED_EVENTS as i64 {
                    insert.execute([timestamp]).unwrap();
                }
            }
            transaction.commit().unwrap();
        }

        assert!(
            store
                .record_sample(&SessionSample {
                    session_id: "bounded-storage".into(),
                    recorded_at_unix: 50,
                    frames_sent: 1,
                    bytes_sent: 2,
                    dropped_frames: 0,
                    actual_fps: 60.0,
                    actual_bitrate_kbps: 10_000.0,
                    encode_latency_ms: Some(1.0),
                })
                .unwrap()
        );
        assert!(
            store
                .record_event(&event("bounded-storage", 50, json!(null)))
                .unwrap()
        );
        assert!(
            store
                .end_session("bounded-storage", 10_002, 10_002)
                .unwrap()
        );
        assert_eq!(store.status().unwrap().samples, MAX_STORED_SAMPLES);
        assert_eq!(store.status().unwrap().events, MAX_STORED_EVENTS);
        let oldest_sample: i64 = store
            .connection
            .query_row(
                "SELECT min(recorded_at_unix) FROM samples WHERE session_id = 'bounded-storage'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(oldest_sample, 50);

        drop(store);
        let mut reopened = HistoryStore::open(&path, 20_000).unwrap();
        configure(&mut reopened, HistorySettings::default(), 20_000);
        let detail = reopened.session_detail("bounded-storage").unwrap();
        assert_eq!(detail.sample_count, MAX_STORED_SAMPLES);
        assert_eq!(detail.event_count, MAX_STORED_EVENTS);
        assert_eq!(detail.samples.last().unwrap().recorded_at_unix, 50);
        assert_eq!(detail.events[detail.events.len() - 2].recorded_at_unix, 50);
        assert_eq!(detail.events.last().unwrap().event_type, "stream_ended");
    }

    #[test]
    fn disabled_restart_defers_overfull_trim_until_writer_reenabled() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        {
            let mut store = HistoryStore::open(&path, 100).unwrap();
            configure(&mut store, HistorySettings::default(), 100);
            store.begin_session(&metadata("legacy-large"), 100).unwrap();
            store.end_session("legacy-large", 200, 200).unwrap();
            store.begin_session(&metadata("small"), 100).unwrap();
            store.end_session("small", 200, 200).unwrap();
            let transaction = store.connection.transaction().unwrap();
            {
                let mut insert = transaction
                    .prepare(
                        "INSERT INTO samples (session_id, recorded_at_unix, frames_sent,
                     bytes_sent, dropped_frames, actual_fps, actual_bitrate_kbps,
                     encode_latency_ms) VALUES ('legacy-large', ?1, 0, 0, 0, 0, 0, 0)",
                    )
                    .unwrap();
                for timestamp in 1..=MAX_STORED_SAMPLES as i64 + 3 {
                    insert.execute([timestamp]).unwrap();
                }
            }
            {
                let mut insert = transaction
                    .prepare(
                        "INSERT INTO events (session_id, recorded_at_unix, event_type, payload)
                     VALUES ('legacy-large', ?1, 'marker', NULL)",
                    )
                    .unwrap();
                for timestamp in 1000..=1002 + MAX_STORED_EVENTS as i64 {
                    insert.execute([timestamp]).unwrap();
                }
            }
            transaction.commit().unwrap();
        }

        let mut reopened = HistoryStore::open(&path, 500).unwrap();
        assert!(!reopened.status().unwrap().configuration_ready);
        assert_eq!(
            reopened
                .session_detail("legacy-large")
                .unwrap()
                .sample_count,
            MAX_STORED_SAMPLES + 3
        );
        assert_eq!(
            reopened.session_detail("legacy-large").unwrap().event_count,
            MAX_STORED_EVENTS + 5
        );
        let oldest_sample: i64 = reopened
            .connection
            .query_row(
                "SELECT min(recorded_at_unix) FROM samples WHERE session_id = 'legacy-large'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(oldest_sample, 1);
        let oldest_event: i64 = reopened
            .connection
            .query_row(
                "SELECT min(recorded_at_unix) FROM events WHERE session_id = 'legacy-large'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(oldest_event, 100);
        assert_eq!(reopened.session_detail("small").unwrap().event_count, 2);

        configure(
            &mut reopened,
            HistorySettings {
                enabled: false,
                ttl_days: 0,
                db_size_limit_mb: 0,
            },
            500,
        );
        assert!(!reopened.settings().1);
        assert_eq!(reopened.status().unwrap().samples, MAX_STORED_SAMPLES + 3);
        assert_eq!(reopened.status().unwrap().events, MAX_STORED_EVENTS + 7);
        assert!(matches!(
            reopened.delete_session("small"),
            Err(HistoryError::RecordingDisabled)
        ));
        configure(&mut reopened, HistorySettings::default(), 501);
        assert!(reopened.writer_open);
        assert_eq!(
            reopened
                .session_detail("legacy-large")
                .unwrap()
                .sample_count,
            MAX_STORED_SAMPLES
        );
        assert_eq!(
            reopened.session_detail("legacy-large").unwrap().event_count,
            MAX_STORED_EVENTS
        );
        reopened.delete_session("small").unwrap();
    }

    #[test]
    fn failed_reenable_trim_keeps_history_readable_and_retries() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        {
            let mut store = HistoryStore::open(&path, 100).unwrap();
            configure(&mut store, HistorySettings::default(), 100);
            store.begin_session(&metadata("interrupted"), 100).unwrap();
            store.end_session("interrupted", 101, 101).unwrap();
            store
                .connection
                .execute_batch(
                    "WITH RECURSIVE numbers(n) AS (
                        SELECT 1 UNION ALL SELECT n + 1 FROM numbers WHERE n < 7201
                     ) INSERT INTO samples (session_id, recorded_at_unix, frames_sent,
                        bytes_sent, dropped_frames, actual_fps, actual_bitrate_kbps,
                        encode_latency_ms)
                     SELECT 'interrupted', n, 0, 0, 0, 0, 0, 0 FROM numbers;
                     CREATE TRIGGER reject_trim BEFORE DELETE ON samples
                     BEGIN SELECT RAISE(ABORT, 'retry trim'); END;",
                )
                .unwrap();
        }

        let mut reopened = HistoryStore::open(&path, 110).unwrap();
        configure(
            &mut reopened,
            HistorySettings {
                enabled: false,
                ..HistorySettings::default()
            },
            110,
        );
        assert!(!reopened.writer_open);
        assert_eq!(
            reopened.session_detail("interrupted").unwrap().sample_count,
            7201
        );
        assert!(matches!(
            reopened.apply_configuration(HistorySettings::default(), 120),
            Err(HistoryError::Database)
        ));
        assert!(!reopened.settings().1);
        assert!(!reopened.writer_open);
        assert_eq!(reopened.status().unwrap().sessions, 1);

        let maintenance = Connection::open(&path).unwrap();
        maintenance
            .execute_batch("DROP TRIGGER reject_trim")
            .unwrap();
        drop(maintenance);
        reopened.prune_if_due(121);
        assert!(reopened.settings().1);
        assert!(reopened.writer_open);
        assert_eq!(
            reopened.session_detail("interrupted").unwrap().sample_count,
            MAX_STORED_SAMPLES
        );
        assert!(reopened.begin_session(&metadata("fresh"), 122).unwrap());
    }

    #[test]
    fn failed_legacy_cleanup_does_not_block_saved_disable() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        store
            .connection
            .execute_batch(
                "CREATE TABLE history_settings (singleton INTEGER PRIMARY KEY, enabled INTEGER,
             ttl_days INTEGER, db_size_limit_mb INTEGER);",
            )
            .unwrap();
        store.begin_session(&metadata("admitted"), 101).unwrap();
        store
            .connection
            .pragma_update(None, "query_only", "ON")
            .unwrap();

        store
            .apply_configuration(
                HistorySettings {
                    enabled: false,
                    ..HistorySettings::default()
                },
                102,
            )
            .unwrap();
        assert!(!store.settings().0.enabled);
        assert!(store.settings().1);
        assert!(matches!(
            store.discard_legacy_settings(),
            Err(HistoryError::Database)
        ));

        store
            .connection
            .pragma_update(None, "query_only", "OFF")
            .unwrap();
        assert!(store.end_session("admitted", 103, 103).unwrap());
        assert!(!store.settings().1);
        assert!(!store.discard_legacy_settings().unwrap());
        assert!(table_exists(&store.connection, "history_settings").unwrap());
        store
            .apply_configuration(HistorySettings::default(), 104)
            .unwrap();
        assert!(store.discard_legacy_settings().unwrap());
    }

    #[test]
    fn failed_read_only_swap_closes_admission_and_retries_after_unrecorded_session() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        configure(&mut store, HistorySettings::default(), 100);
        store.begin_session(&metadata("recorded"), 101).unwrap();
        store
            .apply_configuration(
                HistorySettings {
                    enabled: false,
                    ..HistorySettings::default()
                },
                102,
            )
            .unwrap();
        store.fail_next_read_only_open = true;
        assert!(store.end_session("recorded", 103, 103).unwrap());
        assert!(store.writer_open);
        assert!(!store.settings().1);
        assert!(!store.begin_session(&metadata("unrecorded"), 104).unwrap());
        assert!(!store.end_session("unrecorded", 105, 105).unwrap());
        assert!(!store.writer_open);
        assert!(matches!(
            store.session_detail("unrecorded"),
            Err(HistoryError::NotFound)
        ));
        assert_eq!(store.session_detail("recorded").unwrap().event_count, 2);
    }

    #[test]
    fn status_confirms_revision_only_after_active_and_pending_activity_drains() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        assert_eq!(store.status().unwrap().applied_revision, None);
        store
            .apply_configuration_with_revision(HistorySettings::default(), 1, 100)
            .unwrap();
        assert_eq!(store.status().unwrap().applied_revision, Some(1));

        store.begin_stream_activity("pending", 101).unwrap();
        store
            .apply_configuration_with_revision(
                HistorySettings {
                    enabled: false,
                    ..HistorySettings::default()
                },
                2,
                102,
            )
            .unwrap();
        let pending = store.status().unwrap();
        assert!(!pending.enabled);
        assert!(pending.effective_enabled);
        assert_eq!(pending.applied_revision, Some(1));
        store
            .begin_session_at(&metadata("pending"), 103, 103)
            .unwrap();
        assert_eq!(store.status().unwrap().applied_revision, Some(1));
        store.end_session("pending", 104, 104).unwrap();
        let disabled = store.status().unwrap();
        assert!(!disabled.effective_enabled);
        assert_eq!(disabled.applied_revision, Some(2));
        assert_eq!(store.session_detail("pending").unwrap().event_count, 2);
    }

    #[test]
    fn status_waits_for_drain_on_retention_only_revision_and_restart_load() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        let mut store = HistoryStore::open(&path, 100).unwrap();
        store
            .apply_configuration_with_revision(HistorySettings::default(), 5, 100)
            .unwrap();
        store.begin_session(&metadata("active"), 101).unwrap();
        store
            .apply_configuration_with_revision(
                HistorySettings {
                    ttl_days: 3,
                    ..HistorySettings::default()
                },
                6,
                102,
            )
            .unwrap();
        assert_eq!(store.status().unwrap().applied_revision, Some(5));
        store.end_session("active", 103, 103).unwrap();
        assert_eq!(store.status().unwrap().applied_revision, Some(6));
        store
            .apply_configuration_with_revision(
                HistorySettings {
                    enabled: false,
                    ..HistorySettings::default()
                },
                7,
                104,
            )
            .unwrap();
        assert_eq!(store.status().unwrap().applied_revision, Some(7));
        assert_eq!(store.session_detail("active").unwrap().event_count, 2);
        drop(store);

        let mut reopened = HistoryStore::open(&path, 200).unwrap();
        let unready = reopened.status().unwrap();
        assert!(!unready.configuration_ready);
        assert_eq!(unready.applied_revision, None);
        reopened
            .apply_configuration_with_revision(
                HistorySettings {
                    enabled: false,
                    ..HistorySettings::default()
                },
                8,
                200,
            )
            .unwrap();
        assert_eq!(reopened.status().unwrap().applied_revision, Some(8));
        assert_eq!(reopened.session_detail("active").unwrap().event_count, 2);
        reopened
            .apply_configuration_with_revision(HistorySettings::default(), 9, 201)
            .unwrap();
        assert_eq!(reopened.status().unwrap().applied_revision, Some(9));
        assert_eq!(reopened.session_detail("active").unwrap().event_count, 2);
    }

    #[test]
    fn failed_writer_adoption_cannot_confirm_saved_revision() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        store
            .apply_configuration_with_revision(HistorySettings::default(), 1, 100)
            .unwrap();
        store.fail_next_read_only_open = true;
        assert!(matches!(
            store.apply_configuration_with_revision(
                HistorySettings {
                    enabled: false,
                    ..HistorySettings::default()
                },
                2,
                101,
            ),
            Err(HistoryError::Database)
        ));
        assert_eq!(store.status().unwrap().applied_revision, Some(1));
        assert!(!store.status().unwrap().effective_enabled);
        store.prune_if_due(102);
        assert_eq!(store.status().unwrap().applied_revision, Some(2));
        assert!(!store.writer_open);
    }
}
