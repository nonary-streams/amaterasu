//! Short-lived, lease-scoped cache of already-authorized shortcut discovery.
//! Action admission remains authoritative; this cache grants no action rights.
use amaterasu_extension_api::{Keybinding, LifecycleTarget};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub context: String,
    pub approval_revision: u64,
    pub settings_revisions: BTreeMap<String, u64>,
    pub bindings: Vec<Keybinding>,
}
struct Entry {
    token: String,
    target: LifecycleTarget,
    expires: Instant,
    snapshot: Snapshot,
}
#[derive(Default)]
pub(crate) struct ShortcutCache {
    entry: Option<Entry>,
}

impl ShortcutCache {
    /// Install only after lifecycle and registry authorization, with all values
    /// filtered by the active binding's action grants.
    pub fn install(
        &mut self,
        token: String,
        target: LifecycleTarget,
        snapshot: Snapshot,
        now: Instant,
    ) {
        // A stale discovery reply must not replace a newer cached view.
        let regressed = self.entry.as_ref().is_some_and(|old| {
            old.token == token
                && old.target == target
                && (snapshot.approval_revision < old.snapshot.approval_revision
                    || (snapshot.approval_revision == old.snapshot.approval_revision
                        && snapshot
                            .settings_revisions
                            .iter()
                            .any(|(provider, revision)| {
                                old.snapshot
                                    .settings_revisions
                                    .get(provider)
                                    .is_some_and(|old_revision| revision < old_revision)
                            })))
        });
        if regressed {
            self.entry = None;
            return;
        }
        self.entry = Some(Entry {
            token,
            target,
            expires: now + CACHE_TTL,
            snapshot,
        });
    }
    pub fn get(&self, token: &str, target: &LifecycleTarget, now: Instant) -> Option<&Snapshot> {
        self.entry
            .as_ref()
            .filter(|entry| entry.token == token && &entry.target == target && now < entry.expires)
            .map(|entry| &entry.snapshot)
    }
    /// A consumed key-up remains owned even if a refreshed snapshot has no bindings.
    pub fn can_bypass_keyboard(
        &self,
        token: &str,
        target: &LifecycleTarget,
        now: Instant,
        has_consumed_held: bool,
    ) -> bool {
        !has_consumed_held
            && self
                .get(token, target, now)
                .is_some_and(|snapshot| snapshot.bindings.is_empty())
    }
    pub fn invalidate(&mut self, token: &str) {
        if self
            .entry
            .as_ref()
            .is_some_and(|entry| entry.token == token)
        {
            self.entry = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(generation: u64) -> LifecycleTarget {
        LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation,
        }
    }
    fn snapshot() -> Snapshot {
        Snapshot {
            context: "active".into(),
            approval_revision: 7,
            settings_revisions: [("provider".into(), 3)].into(),
            bindings: vec![],
        }
    }
    #[test]
    fn cache_is_bound_to_exact_generation_token_and_expiry() {
        let now = Instant::now();
        let mut cache = ShortcutCache::default();
        cache.install("token".into(), target(1), snapshot(), now);
        let cached = cache.get("token", &target(1), now).unwrap();
        assert_eq!(cached.approval_revision, 7);
        assert_eq!(cached.settings_revisions["provider"], 3);
        assert!(cache.get("other", &target(1), now).is_none());
        assert!(cache.get("token", &target(2), now).is_none());
        assert!(
            cache
                .get("token", &target(1), now + Duration::from_secs(1))
                .is_none()
        );
        cache.invalidate("other");
        assert!(cache.get("token", &target(1), now).is_some());
        cache.invalidate("token");
        assert!(cache.get("token", &target(1), now).is_none());
    }
    #[test]
    fn stale_discovery_invalidates_cached_view() {
        let now = Instant::now();
        let mut cache = ShortcutCache::default();
        cache.install("token".into(), target(1), snapshot(), now);
        let mut older = snapshot();
        older.settings_revisions.insert("provider".into(), 2);
        cache.install("token".into(), target(1), older, now);
        assert!(cache.get("token", &target(1), now).is_none());
    }
    #[test]
    fn empty_cache_never_bypasses_a_consumed_held_key() {
        let now = Instant::now();
        let mut cache = ShortcutCache::default();
        assert!(!cache.can_bypass_keyboard("token", &target(1), now, false));
        cache.install("token".into(), target(1), snapshot(), now);
        assert!(cache.can_bypass_keyboard("token", &target(1), now, false));
        assert!(!cache.can_bypass_keyboard("token", &target(1), now, true));
    }
}
