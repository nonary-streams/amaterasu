//! Source-owned keyboard plans shared by native adapters.
//! Multiple client keys may resolve to one host key. Its first native plan
//! remains owned until the final source releases it, even after policy changes.

use crate::model::{InputResult, KeyState};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OwnedKeyPlan<Plan> {
    pub target: u16,
    pub native: Plan,
}

pub(super) struct HeldKeyPlans<Plan> {
    sources: HashMap<u8, OwnedKeyPlan<Plan>>,
}

impl<Plan: Copy> HeldKeyPlans<Plan> {
    pub fn new() -> Self {
        Self {
            sources: HashMap::new(),
        }
    }

    pub fn held(&self, source: u8) -> Option<OwnedKeyPlan<Plan>> {
        self.sources.get(&source).copied()
    }

    pub fn target_plan(&self, target: u16) -> Option<OwnedKeyPlan<Plan>> {
        self.sources
            .values()
            .find(|plan| plan.target == target)
            .copied()
    }

    pub fn clear(&mut self) {
        self.sources.clear();
    }

    #[cfg(test)]
    pub fn get(&self, source: &u8) -> Option<&Plan> {
        self.sources.get(source).map(|plan| &plan.native)
    }

    #[cfg(test)]
    pub fn insert(&mut self, source: u8, plan: OwnedKeyPlan<Plan>) {
        self.sources.insert(source, plan);
    }

    #[cfg(test)]
    pub fn contains_key(&self, source: &u8) -> bool {
        self.sources.contains_key(source)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn apply(
        &mut self,
        source: u8,
        state: KeyState,
        proposed: OwnedKeyPlan<Plan>,
        mut inject: impl FnMut(Plan) -> InputResult<()>,
    ) -> InputResult<()> {
        let selected = self
            .held(source)
            .or_else(|| self.target_plan(proposed.target))
            .unwrap_or(proposed);
        let other_owner = self
            .sources
            .iter()
            .any(|(owner, plan)| *owner != source && plan.target == selected.target);
        match state {
            KeyState::Down => {
                // A failed native call may have delivered the down. Preserve
                // the exact plan before writing, so cleanup can retry it.
                self.sources.insert(source, selected);
                if !other_owner {
                    inject(selected.native)?;
                }
            }
            KeyState::Up => {
                if !other_owner {
                    inject(selected.native)?;
                }
                // Keep ownership when the final native release fails.
                self.sources.remove(&source);
            }
            KeyState::Repeat => inject(selected.native)?,
        }
        Ok(())
    }
}

#[cfg(test)]
impl<Plan: Copy> std::ops::Index<&u8> for HeldKeyPlans<Plan> {
    type Output = Plan;

    fn index(&self, source: &u8) -> &Self::Output {
        self.get(source).expect("source has a held native key plan")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::InputError;

    #[test]
    fn shared_target_retains_first_plan_and_releases_only_its_final_owner() {
        let mut keys = HeldKeyPlans::new();
        let first = OwnedKeyPlan {
            target: 30,
            native: "first representation",
        };
        let changed = OwnedKeyPlan {
            target: 30,
            native: "changed representation",
        };
        let mut writes = Vec::new();
        keys.apply(0x41, KeyState::Down, first, |plan| {
            writes.push((1, plan));
            Ok(())
        })
        .unwrap();
        keys.apply(0x42, KeyState::Down, changed, |plan| {
            writes.push((1, plan));
            Ok(())
        })
        .unwrap();
        assert_eq!(keys.held(0x42), Some(first));
        assert_eq!(keys.len(), 2);
        keys.apply(0x42, KeyState::Repeat, changed, |plan| {
            writes.push((2, plan));
            Ok(())
        })
        .unwrap();
        keys.apply(0x41, KeyState::Up, changed, |plan| {
            writes.push((0, plan));
            Ok(())
        })
        .unwrap();
        assert!(keys.held(0x41).is_none());
        assert!(keys.held(0x42).is_some());
        keys.apply(0x42, KeyState::Up, changed, |plan| {
            writes.push((0, plan));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            writes,
            [
                (1, "first representation"),
                (2, "first representation"),
                (0, "first representation")
            ]
        );
        assert!(keys.sources.is_empty());
    }

    #[test]
    fn ambiguous_down_and_failed_final_release_preserve_source_ownership() {
        let mut keys = HeldKeyPlans::new();
        let first = OwnedKeyPlan {
            target: 30,
            native: 30,
        };
        assert!(
            keys.apply(0x41, KeyState::Down, first, |_| Err(InputError::new(
                "injection_failed",
                "possible down"
            )))
            .is_err()
        );
        assert_eq!(keys.held(0x41), Some(first));
        let changed = OwnedKeyPlan {
            target: 48,
            native: 48,
        };
        assert!(
            keys.apply(0x41, KeyState::Up, changed, |plan| {
                assert_eq!(plan, 30);
                Err(InputError::new("release_failed", "possible up"))
            })
            .is_err()
        );
        assert_eq!(keys.held(0x41), Some(first));
        keys.apply(0x41, KeyState::Up, changed, |plan| {
            assert_eq!(plan, 30);
            Ok(())
        })
        .unwrap();
        assert!(keys.sources.is_empty());
    }
}
