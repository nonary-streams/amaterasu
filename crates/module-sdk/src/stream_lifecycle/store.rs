//! The launch owner's store: one lifecycle per stream.
use super::{IssueDisposition, MAX_LIFECYCLE_ENTRIES, StreamIssue};
use crate::LogEmitter;
use crate::stream_warnings::MAX_WARNINGS;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

/// Streams the store keeps; the oldest is forgotten first.
pub const MAX_TRACKED_STREAMS: usize = 32;
/// Issues kept that named no stream this store knows.
pub const MAX_UNATTRIBUTED_ENTRIES: usize = 32;

/// One committed issue with the identity that reported it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LifecycleEntry {
    /// Store-wide commit order.
    pub seq: u64,
    pub at_ms: u64,
    /// The module that reported the issue, as authenticated by the runtime,
    /// or the owner's own identity for issues it committed directly.
    pub publisher: String,
    pub issue: StreamIssue,
}

/// One stream's lifecycle state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamLifecycle {
    key: String,
    phase: String,
    ended: bool,
    root_cause: Option<LifecycleEntry>,
    warnings: Vec<LifecycleEntry>,
    entries: VecDeque<LifecycleEntry>,
    /// Critical failures their own reporter later recovered from.
    recovered: Vec<LifecycleEntry>,
    committed: u64,
    audit_gap: bool,
}

impl StreamLifecycle {
    fn new(key: String) -> Self {
        Self {
            key,
            phase: "opened".to_owned(),
            ended: false,
            root_cause: None,
            warnings: Vec::new(),
            entries: VecDeque::new(),
            recovered: Vec::new(),
            committed: 0,
            audit_gap: false,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn phase(&self) -> &str {
        &self.phase
    }

    pub fn healthy(&self) -> bool {
        self.root_cause.is_none()
    }

    /// The first critical failure. Later failures, such as cleanup errors
    /// caused by it, never replace it.
    pub fn root_cause(&self) -> Option<&LifecycleEntry> {
        self.root_cause.as_ref()
    }

    pub fn warnings(&self) -> &[LifecycleEntry] {
        &self.warnings
    }

    pub fn entries(&self) -> impl Iterator<Item = &LifecycleEntry> {
        self.entries.iter()
    }

    /// True when issues may have been missed because the subscription fell
    /// behind while this stream was open.
    pub fn audit_gap(&self) -> bool {
        self.audit_gap
    }

    fn apply(&mut self, entry: LifecycleEntry) {
        self.committed = self.committed.saturating_add(1);
        match entry.issue.disposition {
            IssueDisposition::CriticalFailure => {
                if outranks(&entry, self.root_cause.as_ref()) {
                    self.root_cause = Some(entry.clone());
                }
            }
            IssueDisposition::DemotedWarning | IssueDisposition::AdvisoryWarning => {
                if entry.issue.disposition == IssueDisposition::DemotedWarning {
                    self.recover(&entry);
                }
                let repeated = self.warnings.iter().any(|existing| {
                    existing.issue.code == entry.issue.code
                        && existing.issue.layer == entry.issue.layer
                        && existing.issue.phase == entry.issue.phase
                });
                if !repeated && self.warnings.len() < MAX_WARNINGS {
                    self.warnings.push(entry.clone());
                }
            }
        }
        let position = self
            .entries
            .iter()
            .rposition(|existing| existing.seq < entry.seq)
            .map_or(0, |index| index + 1);
        self.entries.insert(position, entry);
        if self.entries.len() > MAX_LIFECYCLE_ENTRIES {
            self.entries.pop_front();
        }
    }

    /// A demoted issue says an earlier critical failure was recovered from
    /// and the stream continued: either the same reporter recovered (same
    /// operation and code), or a caller fell back from it (`caused_by` its
    /// layer, with its code as the reason). It stops being the root cause, and the next
    /// unrecovered failure, if any, takes its place.
    fn recover(&mut self, demotion: &LifecycleEntry) {
        let recovers = |failure: &LifecycleEntry| {
            let own_recovery = failure.publisher == demotion.publisher
                && failure.issue.operation == demotion.issue.operation
                && failure.issue.code == demotion.issue.code;
            // A caller that fell back from a dependency's failure says so
            // with `caused_by` that layer and the dependency's code.
            let callers_fallback = failure.issue.is_origin()
                && demotion.issue.caused_by == Some(failure.issue.layer)
                && demotion.issue.reason == failure.issue.code;
            failure.issue.is_critical()
                && failure.seq < demotion.seq
                && (own_recovery || callers_fallback)
        };
        if !self.root_cause.as_ref().is_some_and(recovers) {
            return;
        }
        if let Some(root) = self.root_cause.take() {
            self.recovered.push(root);
        }
        let mut root: Option<&LifecycleEntry> = None;
        for failure in self
            .entries
            .iter()
            .filter(|entry| entry.issue.is_critical())
        {
            let superseded = recovers(failure)
                || self
                    .recovered
                    .iter()
                    .any(|recovered| recovered.seq == failure.seq);
            if !superseded && outranks(failure, root) {
                root = Some(failure);
            }
        }
        self.root_cause = root.cloned();
    }

    pub fn snapshot(&self) -> StreamLifecycleSnapshot {
        StreamLifecycleSnapshot {
            phase: self.phase.clone(),
            ended: self.ended,
            healthy: self.healthy(),
            root_cause: self.root_cause.clone(),
            warnings: self.warnings.clone(),
            committed: self.committed,
            entries_dropped: self.committed.saturating_sub(self.entries.len() as u64),
            audit_gap: self.audit_gap,
            entries: self.entries.iter().cloned().collect(),
        }
    }
}

/// The root cause is the earliest failure that originated in its reporter.
/// A caller's reaction to a dependency (`caused_by`) stands in only until the
/// dependency's own report arrives. Commit order, not arrival, decides, so an
/// earlier report filed late by a binding still wins.
fn outranks(entry: &LifecycleEntry, root: Option<&LifecycleEntry>) -> bool {
    let Some(root) = root else {
        return true;
    };
    match (entry.issue.is_origin(), root.issue.is_origin()) {
        (true, false) => true,
        (false, true) => false,
        _ => entry.seq < root.seq,
    }
}

/// What a getter returns for one stream. It carries no stream key, so an
/// owner can serve it beside its own opaque reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StreamLifecycleSnapshot {
    pub phase: String,
    pub ended: bool,
    pub healthy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<LifecycleEntry>,
    pub warnings: Vec<LifecycleEntry>,
    pub committed: u64,
    pub entries_dropped: u64,
    pub audit_gap: bool,
    pub entries: Vec<LifecycleEntry>,
}

/// Every tracked stream's lifecycle, kept by the launch owner.
///
/// Issues are matched to a stream by operation: the stream key itself, an
/// alias the owner registered, or an ID that starts with either followed by
/// `-`, `.` or `:`. An issue that matches nothing is kept, bounded, as
/// unattributed instead of being discarded.
#[derive(Clone, Debug, Default)]
pub struct StreamLifecycleStore {
    streams: BTreeMap<String, StreamLifecycle>,
    order: VecDeque<String>,
    aliases: BTreeMap<String, String>,
    unattributed: VecDeque<LifecycleEntry>,
    next_seq: u64,
    gaps: u64,
}

impl StreamLifecycleStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start tracking `key`. Opening a tracked stream changes nothing.
    pub fn open(&mut self, key: &str) {
        if key.is_empty() || self.streams.contains_key(key) {
            return;
        }
        while self.order.len() >= MAX_TRACKED_STREAMS {
            if let Some(oldest) = self.order.pop_front() {
                self.streams.remove(&oldest);
                self.aliases.retain(|_, stream| *stream != oldest);
            }
        }
        self.order.push_back(key.to_owned());
        self.streams
            .insert(key.to_owned(), StreamLifecycle::new(key.to_owned()));
    }

    /// Track a short-lived refusal without evicting a still-open stream.
    /// If every slot is open, its issue can still be committed as unattributed.
    pub fn open_transient(&mut self, key: &str) -> bool {
        if key.is_empty() {
            return false;
        }
        if self.streams.contains_key(key) {
            return true;
        }
        if self.order.len() >= MAX_TRACKED_STREAMS {
            let Some(index) = self.order.iter().position(|candidate| {
                self.streams
                    .get(candidate)
                    .is_some_and(|stream| stream.ended)
            }) else {
                return false;
            };
            let oldest = self.order.remove(index).expect("known stream index");
            self.streams.remove(&oldest);
            self.aliases.retain(|_, stream| *stream != oldest);
        }
        self.open(key);
        true
    }

    /// Let issues naming `alias`, or an ID derived from it, reach `key`.
    pub fn alias(&mut self, key: &str, alias: &str) {
        if alias.is_empty() || alias == key || !self.streams.contains_key(key) {
            return;
        }
        self.aliases.insert(alias.to_owned(), key.to_owned());
    }

    /// Let issues naming `alias`, or an ID derived from it, reach the stream
    /// `stream` resolves to. Issues already kept as unattributed that now
    /// resolve are moved to that stream, so a provider's report may arrive
    /// before the binding that names its stream.
    pub fn bind(&mut self, stream: &str, alias: &str) {
        let Some(key) = self.resolve(stream).map(str::to_owned) else {
            return;
        };
        self.alias(&key, alias);
        let waiting = std::mem::take(&mut self.unattributed);
        for entry in waiting {
            match self.resolve(&entry.issue.operation).map(str::to_owned) {
                Some(key) => {
                    if let Some(stream) = self.streams.get_mut(&key) {
                        stream.apply(entry);
                    }
                }
                None => self.unattributed.push_back(entry),
            }
        }
    }

    pub fn enter_phase(&mut self, key: &str, phase: &str) {
        if let Some(stream) = self.streams.get_mut(key)
            && stream.phase != phase
        {
            phase.clone_into(&mut stream.phase);
        }
    }

    /// The stream ended; its record is kept until it is the oldest.
    pub fn end(&mut self, key: &str) {
        if let Some(stream) = self.streams.get_mut(key) {
            stream.ended = true;
        }
    }

    pub fn stream(&self, key: &str) -> Option<&StreamLifecycle> {
        self.streams.get(key)
    }

    pub fn snapshot(&self, key: &str) -> Option<StreamLifecycleSnapshot> {
        self.streams.get(key).map(StreamLifecycle::snapshot)
    }

    pub fn unattributed(&self) -> impl Iterator<Item = &LifecycleEntry> {
        self.unattributed.iter()
    }

    /// Times a subscription reported missed events.
    pub fn gaps(&self) -> u64 {
        self.gaps
    }

    /// The stream an operation belongs to, if any.
    pub fn resolve(&self, operation: &str) -> Option<&str> {
        if let Some((key, _)) = self.streams.get_key_value(operation) {
            return Some(key);
        }
        if let Some(key) = self.aliases.get(operation) {
            return Some(key);
        }
        let derived = |prefix: &str| {
            operation.len() > prefix.len()
                && operation.starts_with(prefix)
                && matches!(operation.as_bytes()[prefix.len()], b'-' | b'.' | b':')
        };
        // The longest matching prefix wins, so a stream whose key extends
        // another's is never credited to the shorter one.
        let stream = self
            .streams
            .keys()
            .filter(|key| derived(key))
            .map(|key| (key.len(), key.as_str()));
        let aliased = self
            .aliases
            .iter()
            .filter(|(alias, _)| derived(alias))
            .map(|(alias, key)| (alias.len(), key.as_str()));
        stream
            .chain(aliased)
            .max_by_key(|(length, _)| *length)
            .map(|(_, key)| key)
    }

    /// Commit `issue`, reported by `publisher`, at the current time.
    pub fn commit(&mut self, publisher: &str, issue: StreamIssue) -> LifecycleEntry {
        self.commit_at(now_epoch_ms(), publisher, issue)
    }

    pub fn commit_at(&mut self, at_ms: u64, publisher: &str, issue: StreamIssue) -> LifecycleEntry {
        self.next_seq = self.next_seq.saturating_add(1);
        let entry = LifecycleEntry {
            seq: self.next_seq,
            at_ms,
            publisher: publisher.to_owned(),
            issue,
        };
        let key = self.resolve(&entry.issue.operation).map(str::to_owned);
        match key.and_then(|key| self.streams.get_mut(&key)) {
            Some(stream) => stream.apply(entry.clone()),
            None => {
                if self.unattributed.len() >= MAX_UNATTRIBUTED_ENTRIES {
                    self.unattributed.pop_front();
                }
                self.unattributed.push_back(entry.clone());
            }
        }
        entry
    }

    /// Commit and log in one step, for an owner committing its own issues.
    pub fn commit_logged(
        &mut self,
        logger: Option<&LogEmitter>,
        publisher: &str,
        issue: StreamIssue,
    ) -> LifecycleEntry {
        issue.log(logger);
        self.commit(publisher, issue)
    }

    /// A subscription missed events. Every stream still open may lack
    /// issues, and says so in its snapshot.
    pub fn note_gap(&mut self) {
        self.gaps = self.gaps.saturating_add(1);
        for stream in self.streams.values_mut().filter(|stream| !stream.ended) {
            stream.audit_gap = true;
        }
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
