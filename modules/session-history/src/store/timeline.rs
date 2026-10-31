//! Stable, bounded views over admitted history rows. Cursors pin row-ID ranges;
//! appends are allowed, while pruning inside a captured range invalidates it.
use super::{HistoryError, HistoryStore, MAX_DETAIL_RESPONSE_BYTES, read_summary, validate_text};
use crate::model::{
    MAX_SESSION_ID_BYTES, MAX_TIMELINE_PAGE_EVENTS, MAX_TIMELINE_PAGE_SAMPLES,
    MAX_TIMELINE_WINDOW_SAMPLES, SessionEvent, SessionSample, TimelineCursor, TimelineKind,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const MAX_EXACT_BROWSER_INTEGER: u64 = 9_007_199_254_740_991;

impl HistoryStore {
    pub fn timeline_open(
        &self,
        session_id: &str,
        sample_limit: Option<usize>,
    ) -> Result<Value, HistoryError> {
        validate_session(session_id)?;
        if sample_limit.is_some_and(|limit| !(1..=MAX_TIMELINE_WINDOW_SAMPLES).contains(&limit)) {
            return Err(HistoryError::Invalid(
                "timeline sample window limit is out of range",
            ));
        }
        // Capture both row ranges and the session metadata in one read transaction.
        let transaction = self.connection.unchecked_transaction()?;
        let session = read_summary(&transaction, session_id)?.ok_or(HistoryError::NotFound)?;
        let samples = capture_window(
            &transaction,
            session_id,
            TimelineKind::Samples,
            sample_limit,
        )?;
        let events = capture_window(&transaction, session_id, TimelineKind::Events, None)?;
        transaction.commit()?;
        Ok(json!({"ok":true,"session":session,"samples":samples,"events":events}))
    }

    pub fn timeline_page(
        &self,
        session_id: &str,
        kind: TimelineKind,
        cursor: TimelineCursor,
        limit: Option<usize>,
    ) -> Result<Value, HistoryError> {
        validate_session(session_id)?;
        let maximum = match kind {
            TimelineKind::Samples => MAX_TIMELINE_PAGE_SAMPLES,
            TimelineKind::Events => MAX_TIMELINE_PAGE_EVENTS,
        };
        let limit = limit.unwrap_or(maximum);
        if !(1..=maximum).contains(&limit) {
            return Err(HistoryError::Invalid("timeline page limit is out of range"));
        }
        let first = parse_id(&cursor.first_id)?;
        let last = parse_id(&cursor.last_id)?;
        let after = cursor.after_id.as_deref().map(parse_id).transpose()?;
        if cursor.session_id != session_id
            || cursor.kind != kind
            || cursor.total == 0
            || cursor.total > MAX_EXACT_BROWSER_INTEGER
            || first > last
            || after.is_some_and(|value| value < first || value >= last)
        {
            return Err(HistoryError::Invalid("timeline cursor is invalid"));
        }
        let transaction = self.connection.unchecked_transaction()?;
        if read_summary(&transaction, session_id)?.is_none() {
            return Err(HistoryError::NotFound);
        }
        let (actual_first, actual_last, actual_total) =
            window_stats(&transaction, session_id, kind, Some((first, last)))?;
        if actual_first != Some(first) || actual_last != Some(last) || actual_total != cursor.total
        {
            return Err(HistoryError::HistoryChanged);
        }
        if let Some(after_id) = after {
            let sql = format!(
                "SELECT 1 FROM {} WHERE session_id = ?1 AND id = ?2",
                table_name(kind)
            );
            if transaction
                .query_row(&sql, params![session_id, after_id], |_| Ok(()))
                .optional()?
                .is_none()
            {
                return Err(HistoryError::Invalid(
                    "timeline cursor does not identify a row",
                ));
            }
        }
        let rows = read_rows(&transaction, session_id, kind, first, last, after, limit)?;
        let mut entries = Vec::new();
        let mut entry_ids = Vec::new();
        let mut response = None;
        for (id, entry) in rows {
            entries.push(entry);
            entry_ids.push(id.to_string());
            let next_cursor = if id == last {
                None
            } else {
                Some(TimelineCursor {
                    after_id: Some(id.to_string()),
                    ..cursor.clone()
                })
            };
            let candidate = json!({"ok":true,"session_id":session_id,"kind":kind,"total":cursor.total,"entries":entries,"entry_ids":entry_ids,"next_cursor":next_cursor});
            if serde_json::to_vec(&candidate)
                .map_err(|_| HistoryError::Database)?
                .len()
                > MAX_DETAIL_RESPONSE_BYTES
            {
                break;
            }
            response = Some(candidate);
        }
        transaction.commit()?;
        response.ok_or(HistoryError::Database)
    }
}

fn validate_session(session_id: &str) -> Result<(), HistoryError> {
    validate_text(session_id, 1, MAX_SESSION_ID_BYTES, "session_id").map_err(HistoryError::Invalid)
}

fn parse_id(value: &str) -> Result<i64, HistoryError> {
    if value.is_empty()
        || value.len() > 19
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(HistoryError::Invalid("timeline cursor ID is invalid"));
    }
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or(HistoryError::Invalid("timeline cursor ID is invalid"))
}

fn table_name(kind: TimelineKind) -> &'static str {
    match kind {
        TimelineKind::Samples => "samples",
        TimelineKind::Events => "events",
    }
}

fn window_stats(
    connection: &Connection,
    session_id: &str,
    kind: TimelineKind,
    range: Option<(i64, i64)>,
) -> Result<(Option<i64>, Option<i64>, u64), HistoryError> {
    let sql = format!(
        "SELECT min(id), max(id), count(*) FROM {} WHERE session_id = ?1 AND id >= ?2 AND id <= ?3",
        table_name(kind)
    );
    let (first, last) = range.unwrap_or((1, i64::MAX));
    let (min, max, count): (Option<i64>, Option<i64>, i64) =
        connection.query_row(&sql, params![session_id, first, last], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let count = u64::try_from(count).map_err(|_| HistoryError::Database)?;
    if count > MAX_EXACT_BROWSER_INTEGER {
        return Err(HistoryError::Invalid("timeline contains too many rows"));
    }
    Ok((min, max, count))
}

fn capture_window(
    connection: &Connection,
    session_id: &str,
    kind: TimelineKind,
    limit: Option<usize>,
) -> Result<Value, HistoryError> {
    let first_id =
        if let Some(limit) = limit {
            connection.query_row(
            "SELECT id FROM samples WHERE session_id = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2",
            params![session_id, (limit - 1) as i64],
            |row| row.get::<_, i64>(0),
        ).optional()?
        } else {
            None
        };
    let (first, last, total) = window_stats(
        connection,
        session_id,
        kind,
        first_id.map(|id| (id, i64::MAX)),
    )?;
    let cursor = match (first, last) {
        (Some(first), Some(last)) if first > 0 && last >= first && total > 0 => {
            Some(TimelineCursor {
                session_id: session_id.to_owned(),
                kind,
                first_id: first.to_string(),
                last_id: last.to_string(),
                after_id: None,
                total,
            })
        }
        (None, None) if total == 0 => None,
        _ => return Err(HistoryError::Database),
    };
    Ok(json!({"total":total,"cursor":cursor}))
}

fn read_rows(
    connection: &Connection,
    session_id: &str,
    kind: TimelineKind,
    first: i64,
    last: i64,
    after: Option<i64>,
    limit: usize,
) -> Result<Vec<(i64, Value)>, HistoryError> {
    let columns = match kind {
        TimelineKind::Samples => {
            "id, session_id, recorded_at_unix, frames_sent, bytes_sent, dropped_frames, actual_fps, actual_bitrate_kbps, encode_latency_ms"
        }
        TimelineKind::Events => "id, session_id, recorded_at_unix, event_type, payload",
    };
    let sql = format!(
        "SELECT {columns} FROM {} WHERE session_id = ?1 AND id >= ?2 AND id <= ?3 AND id > ?4 ORDER BY id ASC LIMIT ?5",
        table_name(kind)
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query(params![
        session_id,
        first,
        last,
        after.unwrap_or(0),
        limit as i64
    ])?;
    let mut result = Vec::with_capacity(limit);
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let value = match kind {
            TimelineKind::Samples => serde_json::to_value(SessionSample {
                session_id: row.get(1)?,
                recorded_at_unix: row.get(2)?,
                frames_sent: u64::try_from(row.get::<_, i64>(3)?).unwrap_or_default(),
                bytes_sent: u64::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
                dropped_frames: u64::try_from(row.get::<_, i64>(5)?).unwrap_or_default(),
                actual_fps: row.get(6)?,
                actual_bitrate_kbps: row.get(7)?,
                encode_latency_ms: row.get(8)?,
            }),
            TimelineKind::Events => {
                let payload = row
                    .get::<_, Option<String>>(4)?
                    .map(|value| serde_json::from_str(&value))
                    .transpose()
                    .map_err(|_| HistoryError::Database)?;
                serde_json::to_value(SessionEvent {
                    session_id: row.get(1)?,
                    recorded_at_unix: row.get(2)?,
                    event_type: row.get(3)?,
                    payload,
                })
            }
        }
        .map_err(|_| HistoryError::Database)?;
        result.push((id, value));
    }
    Ok(result)
}
