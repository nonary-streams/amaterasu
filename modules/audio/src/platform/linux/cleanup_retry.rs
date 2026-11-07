//! Retains exact-owner cleanup evidence until the durable journal accepts it.
use super::cleanup_journal::{CleanupJournal, CleanupState, MAX_SINK_INTENTS};
use crate::model::AudioResult;
use std::collections::BTreeMap;

pub(super) trait CleanupStore {
    fn read(&self) -> AudioResult<CleanupState>;
    fn write(&self, state: &CleanupState) -> AudioResult<()>;
}

impl CleanupStore for CleanupJournal {
    fn read(&self) -> AudioResult<CleanupState> {
        self.read()
    }

    fn write(&self, state: &CleanupState) -> AudioResult<()> {
        self.write(state)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UnloadEvidence {
    Confirmed,
    Quiesced,
}

#[derive(Debug)]
struct PendingCleanup {
    owner_token: String,
    retirement_requested: bool,
    unload: Option<(u32, UnloadEvidence)>,
}

pub(super) struct SinkCleanupCoordinator<S = CleanupJournal> {
    store: S,
    pending: BTreeMap<String, PendingCleanup>,
}

impl<S: CleanupStore> SinkCleanupCoordinator<S> {
    pub(super) fn new(store: S) -> Self {
        Self {
            store,
            pending: BTreeMap::new(),
        }
    }

    pub(super) fn read(&self) -> AudioResult<CleanupState> {
        self.store.read()
    }

    pub(super) fn has_deferred_cleanup(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(super) fn update(
        &mut self,
        update: impl FnOnce(&mut CleanupState) -> AudioResult<()>,
    ) -> AudioResult<()> {
        let mut state = self.store.read()?;
        for (name, pending) in &self.pending {
            let Some(intent) = state
                .sinks
                .get_mut(name)
                .filter(|intent| intent.owner_token == pending.owner_token)
            else {
                // This exact owner no longer has a durable obligation. Never
                // apply its evidence to a replacement that reused the name.
                continue;
            };
            if pending.retirement_requested {
                intent.lease_active = false;
                intent.cleanup_requested = true;
            }
            if let Some((module_index, evidence)) = pending.unload
                && intent.module_index == Some(module_index)
            {
                match evidence {
                    UnloadEvidence::Confirmed => intent.unload_confirmed = true,
                    UnloadEvidence::Quiesced => intent.unload_quiesced = true,
                }
            }
        }
        update(&mut state)?;
        self.store.write(&state)?;
        // A successful write either persisted the evidence for its exact
        // owner/module or established that owner is no longer present.
        self.pending.clear();
        Ok(())
    }

    pub(super) fn retry_pending(&mut self) -> AudioResult<()> {
        if self.has_deferred_cleanup() {
            self.update(|_| Ok(()))?;
        }
        Ok(())
    }

    fn pending_for(&mut self, name: &str, owner_token: &str) -> AudioResult<&mut PendingCleanup> {
        // Only already admitted journal intents can produce these obligations.
        // Keep the same bound, reject overflow, and never evict unresolved work.
        if !self.pending.contains_key(name) && self.pending.len() >= MAX_SINK_INTENTS {
            return Err("audio_cleanup_journal_capacity_reached");
        }
        let pending = self
            .pending
            .entry(name.to_owned())
            .or_insert_with(|| PendingCleanup {
                owner_token: owner_token.to_owned(),
                retirement_requested: false,
                unload: None,
            });
        if pending.owner_token != owner_token {
            return Err("audio_cleanup_journal_conflict");
        }
        Ok(pending)
    }

    pub(super) fn request_retirement(&mut self, name: &str, owner_token: &str) -> AudioResult<()> {
        // Capture ownership ends before any filesystem operation. Retain that
        // fact even if reading or writing its durable intent currently fails.
        self.pending_for(name, owner_token)?.retirement_requested = true;
        self.retry_pending()
    }

    pub(super) fn record_unload(
        &mut self,
        name: &str,
        owner_token: &str,
        module_index: u32,
        evidence: UnloadEvidence,
    ) -> AudioResult<()> {
        let pending = self.pending_for(name, owner_token)?;
        if pending
            .unload
            .is_some_and(|(expected, _)| expected != module_index)
        {
            return Err("audio_cleanup_journal_conflict");
        }
        // Evidence comes only from the unload callback or an ordered round trip
        // on that command's original client. A replacement connection or an
        // absent observation cannot manufacture it.
        pending.unload = Some((module_index, evidence));
        self.update(|state| {
            state
                .sinks
                .get(name)
                .filter(|intent| {
                    intent.owner_token == owner_token && intent.module_index == Some(module_index)
                })
                .ok_or("audio_cleanup_journal_conflict")?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::cleanup_journal::{
        SinkCleanupAction, SinkIntent, SinkObservation, sink_cleanup_action, sink_cleanup_requested,
    };
    use super::*;
    use std::cell::{Cell, RefCell};

    struct InjectedStore {
        durable: RefCell<CleanupState>,
        fail_write: Cell<bool>,
        fail_read: Cell<bool>,
    }

    impl CleanupStore for InjectedStore {
        fn read(&self) -> AudioResult<CleanupState> {
            if self.fail_read.replace(false) {
                return Err("audio_cleanup_journal_unavailable");
            }
            Ok(self.durable.borrow().clone())
        }

        fn write(&self, state: &CleanupState) -> AudioResult<()> {
            if self.fail_write.replace(false) {
                return Err("audio_cleanup_journal_unavailable");
            }
            *self.durable.borrow_mut() = state.clone();
            Ok(())
        }
    }

    fn coordinator(intent: SinkIntent) -> SinkCleanupCoordinator<InjectedStore> {
        SinkCleanupCoordinator::new(InjectedStore {
            durable: RefCell::new(CleanupState {
                sinks: BTreeMap::from([("owned-sink".into(), intent)]),
                route: None,
            }),
            fail_write: Cell::new(true),
            fail_read: Cell::new(false),
        })
    }

    fn owned_intent() -> SinkIntent {
        SinkIntent {
            owner_token: "owner-1".into(),
            module_index: Some(71),
            lease_active: true,
            load_quiesced: true,
            ..SinkIntent::default()
        }
    }

    #[test]
    fn retirement_write_failure_retries_after_storage_recovers() {
        let mut coordinator = coordinator(owned_intent());
        assert_eq!(
            coordinator.request_retirement("owned-sink", "owner-1"),
            Err("audio_cleanup_journal_unavailable")
        );
        assert!(
            coordinator.has_deferred_cleanup(),
            "a dropped lease still owns a cleanup obligation"
        );
        assert!(coordinator.read().unwrap().sinks["owned-sink"].lease_active);
        coordinator.retry_pending().unwrap();
        assert!(!coordinator.has_deferred_cleanup());
        let state = coordinator.read().unwrap();
        assert!(sink_cleanup_requested(&state.sinks["owned-sink"]));
    }

    #[test]
    fn unload_confirmation_write_failure_retries_exact_evidence() {
        for evidence in [UnloadEvidence::Confirmed, UnloadEvidence::Quiesced] {
            let mut intent = owned_intent();
            intent.lease_active = false;
            intent.cleanup_requested = true;
            intent.unload_submitted = true;
            let mut coordinator = coordinator(intent);
            assert_eq!(
                coordinator.record_unload("owned-sink", "owner-1", 71, evidence),
                Err("audio_cleanup_journal_unavailable")
            );
            assert!(
                coordinator.has_deferred_cleanup(),
                "native acknowledgment must survive a journal write failure"
            );
            assert_eq!(
                sink_cleanup_action(
                    &coordinator.read().unwrap().sinks["owned-sink"],
                    SinkObservation::Absent
                ),
                SinkCleanupAction::KeepTombstone
            );
            coordinator.retry_pending().unwrap();
            assert!(!coordinator.has_deferred_cleanup());
            assert_eq!(
                sink_cleanup_action(
                    &coordinator.read().unwrap().sinks["owned-sink"],
                    SinkObservation::Absent
                ),
                SinkCleanupAction::ClearConfirmedAbsent
            );
        }
    }
    #[test]
    fn deferred_retirement_survives_read_failure_and_repeated_failed_writes() {
        let mut coordinator = coordinator(owned_intent());
        coordinator.store.fail_read.set(true);
        assert!(
            coordinator
                .request_retirement("owned-sink", "owner-1")
                .is_err()
        );
        assert!(coordinator.has_deferred_cleanup());
        for _ in 0..3 {
            coordinator.store.fail_write.set(true);
            assert!(coordinator.retry_pending().is_err());
            assert!(coordinator.has_deferred_cleanup());
            assert!(coordinator.read().unwrap().sinks["owned-sink"].lease_active);
        }
        coordinator.retry_pending().unwrap();
        assert!(sink_cleanup_requested(
            &coordinator.read().unwrap().sinks["owned-sink"]
        ));
        assert!(!coordinator.has_deferred_cleanup());
    }

    #[test]
    fn deferred_cleanup_never_mutates_a_replacement_owner_or_module() {
        for replaced_owner in [false, true] {
            let mut intent = owned_intent();
            intent.unload_submitted = true;
            let mut coordinator = coordinator(intent);
            assert!(
                coordinator
                    .record_unload("owned-sink", "owner-1", 71, UnloadEvidence::Confirmed)
                    .is_err()
            );
            // Model another native owner, or a new native module instance with
            // the same owner token, replacing the durable record before retry.
            let mut replacement = owned_intent();
            if replaced_owner {
                replacement.owner_token = "owner-2".into();
            } else {
                replacement.module_index = Some(72);
            }
            coordinator
                .store
                .durable
                .borrow_mut()
                .sinks
                .insert("owned-sink".into(), replacement.clone());
            coordinator.retry_pending().unwrap();
            assert_eq!(coordinator.read().unwrap().sinks["owned-sink"], replacement);
            assert!(!coordinator.has_deferred_cleanup());
        }

        let mut coordinator = coordinator(owned_intent());
        assert!(
            coordinator
                .request_retirement("owned-sink", "owner-1")
                .is_err()
        );
        let mut replacement = owned_intent();
        replacement.owner_token = "owner-2".into();
        coordinator
            .store
            .durable
            .borrow_mut()
            .sinks
            .insert("owned-sink".into(), replacement.clone());
        coordinator.retry_pending().unwrap();
        assert_eq!(coordinator.read().unwrap().sinks["owned-sink"], replacement);
    }

    #[test]
    fn deferred_cleanup_keeps_its_capacity_without_evicting_unresolved_owners() {
        let mut coordinator = coordinator(owned_intent());
        for index in 0..MAX_SINK_INTENTS {
            let name = format!("sink-{index}");
            coordinator.store.fail_write.set(true);
            assert!(coordinator.request_retirement(&name, "owner-1").is_err());
        }
        assert_eq!(coordinator.pending.len(), MAX_SINK_INTENTS);
        assert_eq!(
            coordinator.request_retirement("one-too-many", "owner-1"),
            Err("audio_cleanup_journal_capacity_reached")
        );
        assert_eq!(coordinator.pending.len(), MAX_SINK_INTENTS);
        assert!(coordinator.pending.contains_key("sink-0"));
        assert_eq!(
            coordinator.request_retirement("sink-0", "owner-2"),
            Err("audio_cleanup_journal_conflict")
        );
        assert_eq!(coordinator.pending["sink-0"].owner_token, "owner-1");
    }

    #[test]
    fn deferred_evidence_stays_with_its_original_store() {
        let mut original = coordinator(owned_intent());
        assert!(
            original
                .request_retirement("owned-sink", "owner-1")
                .is_err()
        );
        let replacement = coordinator(owned_intent());
        assert!(!replacement.has_deferred_cleanup());
        assert!(replacement.read().unwrap().sinks["owned-sink"].lease_active);
        original.retry_pending().unwrap();
        assert!(sink_cleanup_requested(
            &original.read().unwrap().sinks["owned-sink"]
        ));
        assert!(replacement.read().unwrap().sinks["owned-sink"].lease_active);
    }
}
