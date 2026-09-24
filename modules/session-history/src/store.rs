use crate::model::{
    HistorySettings, HistoryStatus, MAX_ACTIVE_LIMIT, MAX_DETAIL_EVENTS, MAX_DETAIL_SAMPLES,
    MAX_SESSION_LIMIT, PruneReport, SessionDetail, SessionEvent, SessionMetadata, SessionSample,
    SessionSummary, validate_text,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

const SCHEMA_VERSION: i64 = 1;
const SECONDS_PER_DAY: i64 = 86_400;
const BYTES_PER_MEBIBYTE: u64 = 1024 * 1024;

#[derive(Debug)]
pub enum HistoryError {
    Invalid(&'static str),
    Database,
    RevisionConflict(u64),
    NotFound,
    ActiveSession,
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
    connection: Connection,
    settings: HistorySettings,
    settings_revision: u64,
    effective_enabled: bool,
    active_streams: BTreeSet<String>,
    failed_operations: u64,
    last_prune: PruneReport,
}

impl HistoryStore {
    pub fn open(path: &Path, now_unix: i64) -> Result<Self, HistoryError> {
        if !path.is_absolute() {
            return Err(HistoryError::Invalid("database path must be absolute"));
        }
        let parent = path.parent().ok_or(HistoryError::Invalid(
            "database path has no parent directory",
        ))?;
        amaterasu_storage::create_private_directory(parent).map_err(|_| HistoryError::Database)?;

        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
        apply_schema(&connection)?;
        let (settings, settings_revision) = read_settings(&connection)?;
        let mut store = Self {
            effective_enabled: settings.enabled,
            settings,
            settings_revision,
            connection,
            active_streams: BTreeSet::new(),
            failed_operations: 0,
            last_prune: empty_prune_report(),
        };
        store.close_interrupted_sessions(now_unix)?;
        store.prune_best_effort(now_unix);
        Ok(store)
    }

    pub fn settings(&self) -> (&HistorySettings, u64, bool) {
        (
            &self.settings,
            self.settings_revision,
            self.effective_enabled,
        )
    }

    pub fn set_settings(
        &mut self,
        settings: HistorySettings,
        expected_revision: u64,
        now_unix: i64,
    ) -> Result<u64, HistoryError> {
        settings.validate().map_err(HistoryError::Invalid)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual_revision: i64 = transaction.query_row(
            "SELECT revision FROM history_settings WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        if u64::try_from(actual_revision).ok() != Some(expected_revision) {
            return Err(HistoryError::RevisionConflict(
                u64::try_from(actual_revision).unwrap_or(0),
            ));
        }
        let next_revision = expected_revision
            .checked_add(1)
            .filter(|revision| *revision <= i64::MAX as u64)
            .ok_or(HistoryError::Invalid("settings revision is exhausted"))?;
        transaction.execute(
            "UPDATE history_settings
             SET enabled = ?1, ttl_days = ?2, db_size_limit_mb = ?3, revision = ?4
             WHERE singleton = 1",
            params![
                settings.enabled,
                settings.ttl_days,
                settings.db_size_limit_mb,
                next_revision as i64
            ],
        )?;
        transaction.commit()?;

        self.settings = settings;
        self.settings_revision = next_revision;
        if self.active_streams.is_empty() {
            self.effective_enabled = self.settings.enabled;
        }
        self.prune_best_effort(now_unix);
        Ok(self.settings_revision)
    }

    pub fn begin_session(
        &mut self,
        metadata: &SessionMetadata,
        started_at_unix: i64,
    ) -> Result<bool, HistoryError> {
        metadata.validate().map_err(HistoryError::Invalid)?;
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
        transaction.commit()?;
        self.active_streams.insert(metadata.session_id.clone());
        Ok(true)
    }

    pub fn end_session(
        &mut self,
        session_id: &str,
        ended_at_unix: i64,
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
                ended = true;
            }
            transaction.commit()?;
        }
        if was_active {
            self.active_streams.remove(session_id);
        }
        if self.active_streams.is_empty() {
            self.effective_enabled = self.settings.enabled;
        }
        self.prune_best_effort(ended_at_unix);
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
        self.connection.execute(
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
        self.connection.execute(
            "INSERT INTO events (session_id, recorded_at_unix, event_type, payload)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                event.session_id,
                event.recorded_at_unix,
                event.event_type,
                payload
            ],
        )?;
        Ok(true)
    }

    pub fn list_sessions(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionSummary>, HistoryError> {
        if !(1..=MAX_SESSION_LIMIT).contains(&limit) {
            return Err(HistoryError::Invalid("limit must be from 1 through 25"));
        }
        let offset = i64::try_from(offset)
            .map_err(|_| HistoryError::Invalid("offset exceeds SQLite integer range"))?;
        let mut statement = self.connection.prepare(
            "SELECT session_id, protocol, client_name, device_name, app_name, width, height,
                    target_fps, target_bitrate_kbps, codec, started_at_unix, ended_at_unix,
                    duration_seconds
             FROM sessions
             ORDER BY started_at_unix DESC, session_id DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![limit as i64, offset], summary_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn session_detail(&self, session_id: &str) -> Result<SessionDetail, HistoryError> {
        let session = read_summary(&self.connection, session_id)?.ok_or(HistoryError::NotFound)?;
        let sample_count = child_count(&self.connection, "samples", session_id)?;
        let event_count = child_count(&self.connection, "events", session_id)?;
        let samples = read_samples(&self.connection, session_id)?;
        let events = read_events(&self.connection, session_id)?;
        Ok(SessionDetail {
            session,
            sample_count,
            event_count,
            samples_truncated: sample_count > samples.len(),
            events_truncated: event_count > events.len(),
            samples,
            events,
        })
    }

    pub fn active_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, HistoryError> {
        if !(1..=MAX_ACTIVE_LIMIT).contains(&limit) {
            return Err(HistoryError::Invalid("limit must be from 1 through 16"));
        }
        let mut statement = self.connection.prepare(
            "SELECT session_id, protocol, client_name, device_name, app_name, width, height,
                    target_fps, target_bitrate_kbps, codec, started_at_unix, ended_at_unix,
                    duration_seconds
             FROM sessions WHERE ended_at_unix IS NULL
             ORDER BY started_at_unix DESC, session_id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], summary_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_session(&mut self, session_id: &str) -> Result<(), HistoryError> {
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
        if !self.effective_enabled {
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
            enabled: self.settings.enabled,
            effective_enabled: self.effective_enabled,
            degraded: self.failed_operations > 0 || self.last_prune.size_limit_unmet,
            failed_operations: self.failed_operations,
            active_streams: self.active_streams.len(),
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

    fn prune_best_effort(&mut self, now_unix: i64) {
        if self.effective_enabled && self.prune(now_unix).is_err() {
            self.record_failure();
        }
    }

    fn close_interrupted_sessions(&mut self, now_unix: i64) -> Result<(), HistoryError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
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
        }
        transaction.commit()?;
        Ok(())
    }
}

fn apply_schema(connection: &Connection) -> Result<(), HistoryError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(HistoryError::Database);
    }
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS history_settings (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
            ttl_days INTEGER NOT NULL CHECK (ttl_days >= 0),
            db_size_limit_mb INTEGER NOT NULL CHECK (db_size_limit_mb >= 0),
            revision INTEGER NOT NULL CHECK (revision >= 1)
         );
         CREATE TABLE IF NOT EXISTS sessions (
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
            encode_latency_ms REAL NOT NULL
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
    connection.execute(
        "INSERT OR IGNORE INTO history_settings
         (singleton, enabled, ttl_days, db_size_limit_mb, revision)
         VALUES (1, 1, 0, 0, 1)",
        [],
    )?;
    connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn read_settings(connection: &Connection) -> Result<(HistorySettings, u64), HistoryError> {
    let (enabled, ttl_days, db_size_limit_mb, revision): (bool, i32, i32, i64) = connection
        .query_row(
            "SELECT enabled, ttl_days, db_size_limit_mb, revision
             FROM history_settings WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let settings = HistorySettings {
        enabled,
        ttl_days,
        db_size_limit_mb,
    };
    settings.validate().map_err(HistoryError::Invalid)?;
    let revision = u64::try_from(revision).map_err(|_| HistoryError::Database)?;
    Ok((settings, revision))
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
    let mut statement = connection.prepare(
        "SELECT session_id, recorded_at_unix, frames_sent, bytes_sent, dropped_frames,
                actual_fps, actual_bitrate_kbps, encode_latency_ms
         FROM (
            SELECT * FROM samples WHERE session_id = ?1
            ORDER BY recorded_at_unix DESC, id DESC LIMIT ?2
         ) ORDER BY recorded_at_unix ASC, id ASC",
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
            ORDER BY recorded_at_unix DESC, id DESC LIMIT ?2
         ) ORDER BY recorded_at_unix ASC, id ASC",
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
    fn session_samples_events_and_settings_survive_database_reopen() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        {
            let mut store = HistoryStore::open(&path, 100).unwrap();
            assert_eq!(store.settings().1, 1);
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
                        encode_latency_ms: 4.25,
                    })
                    .unwrap()
            );
            assert!(
                store
                    .record_event(&event("session-1", 106, json!({"marker":"test"})))
                    .unwrap()
            );
            assert!(store.end_session("session-1", 110).unwrap());
            assert_eq!(
                store
                    .set_settings(
                        HistorySettings {
                            enabled: true,
                            ttl_days: 12,
                            db_size_limit_mb: 8,
                        },
                        1,
                        111,
                    )
                    .unwrap(),
                2
            );
        }

        let store = HistoryStore::open(&path, 120).unwrap();
        let (settings, revision, effective_enabled) = store.settings();
        assert_eq!(settings.ttl_days, 12);
        assert_eq!(settings.db_size_limit_mb, 8);
        assert_eq!(revision, 2);
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
    fn reopening_marks_unclosed_sessions_as_interrupted() {
        let database = TemporaryDatabase::new();
        let path = database.path();
        {
            let mut store = HistoryStore::open(&path, 100).unwrap();
            store.begin_session(&metadata("interrupted"), 90).unwrap();
        }

        let store = HistoryStore::open(&path, 150).unwrap();
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
            store.end_session("retry-end", 110),
            Err(HistoryError::Database)
        ));
        assert_eq!(store.status().unwrap().active_streams, 1);
        assert_eq!(store.active_sessions(16).unwrap().len(), 1);
        store
            .connection
            .execute_batch("DROP TRIGGER reject_stream_ended")
            .unwrap();
        assert!(store.end_session("retry-end", 110).unwrap());
        assert_eq!(store.status().unwrap().active_streams, 0);
    }

    #[test]
    fn enabled_changes_wait_until_the_last_active_session_ends() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        store
            .set_settings(
                HistorySettings {
                    enabled: false,
                    ..HistorySettings::default()
                },
                1,
                100,
            )
            .unwrap();
        assert!(!store.begin_session(&metadata("unrecorded"), 110).unwrap());
        store
            .set_settings(HistorySettings::default(), 2, 120)
            .unwrap();
        assert!(!store.settings().2);
        assert!(!store.end_session("unrecorded", 130).unwrap());
        assert!(store.settings().2);
        assert!(store.list_sessions(25, 0).unwrap().is_empty());
    }

    #[test]
    fn age_pruning_removes_expired_ended_sessions_and_preserves_active_sessions() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 200_000).unwrap();
        store
            .set_settings(
                HistorySettings {
                    enabled: true,
                    ttl_days: 1,
                    db_size_limit_mb: 0,
                },
                1,
                200_000,
            )
            .unwrap();
        store.begin_session(&metadata("expired"), 100).unwrap();
        store.begin_session(&metadata("recent"), 190_000).unwrap();
        store.begin_session(&metadata("active"), 100).unwrap();
        store.end_session("expired", 101).unwrap();
        store.end_session("recent", 190_100).unwrap();

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
    fn size_pruning_removes_oldest_ended_sessions_without_deleting_active_records() {
        let database = TemporaryDatabase::new();
        let mut store = HistoryStore::open(&database.path(), 100).unwrap();
        store.begin_session(&metadata("oldest"), 10).unwrap();
        for index in 0..150 {
            store
                .record_event(&event("oldest", 11 + index, json!("x".repeat(4000))))
                .unwrap();
        }
        store.end_session("oldest", 200).unwrap();
        store.begin_session(&metadata("newer"), 300).unwrap();
        for index in 0..150 {
            store
                .record_event(&event("newer", 301 + index, json!("y".repeat(4000))))
                .unwrap();
        }
        store.end_session("newer", 500).unwrap();
        store.begin_session(&metadata("still-active"), 600).unwrap();

        let report = store
            .set_settings(
                HistorySettings {
                    enabled: true,
                    ttl_days: 0,
                    db_size_limit_mb: 1,
                },
                1,
                601,
            )
            .map(|_| store.status().unwrap().last_prune)
            .unwrap();
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
        store.begin_session(&metadata("active"), 10).unwrap();
        assert!(matches!(
            store.delete_session("active"),
            Err(HistoryError::ActiveSession)
        ));
        store
            .record_event(&event("active", 11, json!({"persist":true})))
            .unwrap();
        store.end_session("active", 12).unwrap();
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
                    encode_latency_ms: 1.0,
                })
                .unwrap();
        }
        for index in 0..12 {
            store
                .record_event(&event("bounded", 60 + index, json!({"index":index})))
                .unwrap();
        }
        store.end_session("bounded", 80).unwrap();

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
}
