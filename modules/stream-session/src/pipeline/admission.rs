//! Admission of a new session reads open sessions only.
//!
//! A session that was asked to stop no longer counts against the session
//! limit, whatever became of its cleanup, and ended sessions are dropped from
//! the record table when it fills. Until this existed every session of a
//! runtime kept its record, so the table refused sessions once it had seen
//! 128 of them.
use super::*;
use amaterasu_module_sdk::lifecycle_ledger::{Admission, Live, Owner, make_room};

pub(super) const MAX_RECORDS: usize = 128;

/// Whether one more session may be admitted. It receives open sessions only,
/// so a session that is stopping or failed to stop cannot use up the limit.
pub(super) fn session_admitted(live: &Live<'_, Record>, limit: usize) -> bool {
    live.admits(limit)
}

impl Pipeline {
    /// Tell the journal which entries it may forget once it is full: first
    /// those no record remains for, then those of ended sessions, the most
    /// settled first. A session that is open, being worked on or waiting on
    /// a journal write in doubt is never listed.
    pub(super) fn refresh_forgettable_journal_entries(&mut self) {
        self.journal.forgettable.clear();
        let entries = &self.journal.document.entries;
        if entries.len() < journal::MAX_ENTRIES {
            return;
        }
        let working = self.job.as_ref().map(|job| job.session);
        let mut ended: Vec<_> = self
            .records
            .values()
            .filter(|record| {
                record.admission() == Admission::Ended
                    && entries.contains_key(&record.id)
                    && record.journal_uncertain.is_none()
                    && Some(record.id) != working
            })
            .map(|record| (record.retirement(), record.id))
            .collect();
        ended.sort();
        let orphaned = entries
            .keys()
            .copied()
            .filter(|id| !self.records.contains_key(id) && Some(*id) != working);
        let forgettable: Vec<u64> = orphaned
            .chain(ended.into_iter().map(|(_, id)| id))
            .collect();
        self.journal.forgettable = forgettable;
    }

    /// Keep space for the next session by dropping ended ones, the most
    /// settled first. A record stays while anything still refers to it: a job,
    /// startup recovery, a journal write in doubt, a retained broker call,
    /// history that is still being written or a borrowed capture source. The
    /// durable journal keeps the cleanup identity of a dropped session, so a
    /// later runtime still recovers it.
    pub(super) fn keep_session_capacity(&mut self) {
        if self.records.len() < MAX_RECORDS {
            return;
        }
        let working = self.job.as_ref().map(|job| job.session);
        let lent: BTreeSet<&str> = self
            .records
            .values()
            .filter(|record| !record.state.terminal())
            .filter_map(|record| record.config.shared_source_operation_id.as_deref())
            .collect();
        let mut pinned: BTreeSet<u64> = self
            .records
            .values()
            .filter(|record| lent.contains(record.config.operation_id.as_str()))
            .map(|record| record.id)
            .collect();
        pinned.extend(working);
        pinned.extend(self.recovery_ids.iter().copied());
        pinned.extend(self.recovery_queue.iter().copied());
        pinned.extend(self.deferred_journal_stops.iter().copied());
        pinned.extend(self.journal_waiting_stops.iter().copied());
        make_room(
            &mut self.records,
            MAX_RECORDS,
            |id, record| {
                !pinned.contains(id)
                    && record.uncertain.is_none()
                    && record.journal_uncertain.is_none()
                    && !record.shared_capture_attached
                    && !record.history.has_work()
            },
            |id, record| {
                if !record.state.terminal() || record.cleanup_error.is_some() {
                    eprintln!(
                        "stream session record dropped for capacity session={id} state={:?} \
                         cleanup_error={}",
                        record.state,
                        record.cleanup_error.unwrap_or("none")
                    );
                }
            },
        );
    }
}
