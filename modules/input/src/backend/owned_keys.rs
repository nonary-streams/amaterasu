//! Source-owned keyboard plans shared by native adapters.
//! Multiple client keys may resolve to one host key. Its first native plan
//! remains owned until the final source releases it, even after policy changes.

use crate::model::{InputResult, KeyState};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostModifier {
    Shift,
    Control,
    Alt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OwnedKeyPlan<Plan> {
    pub target: u16,
    pub native: Plan,
    /// Group of the resolved native destination, captured with its first plan.
    pub modifier: Option<HostModifier>,
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

    fn modifier_plan(&self, modifier: HostModifier) -> Option<OwnedKeyPlan<Plan>> {
        self.sources
            .values()
            .find(|plan| plan.modifier == Some(modifier))
            .copied()
    }

    pub fn select(
        &self,
        source: u8,
        proposed: OwnedKeyPlan<Plan>,
        temporary_modifier: bool,
    ) -> OwnedKeyPlan<Plan> {
        self.held(source)
            .or_else(|| self.target_plan(proposed.target))
            .or_else(|| {
                // Only a synthetic modifier may reuse an owned opposite side.
                // The saved host group follows its native plan across remaps.
                if temporary_modifier {
                    proposed
                        .modifier
                        .and_then(|group| self.modifier_plan(group))
                } else {
                    None
                }
            })
            .unwrap_or(proposed)
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
        temporary_modifier: bool,
        mut inject: impl FnMut(Plan) -> InputResult<()>,
    ) -> InputResult<()> {
        let selected = self.select(source, proposed, temporary_modifier);
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
            modifier: None,
        };
        let changed = OwnedKeyPlan {
            target: 30,
            native: "changed representation",
            modifier: None,
        };
        let mut writes = Vec::new();
        keys.apply(0x41, KeyState::Down, first, false, |plan| {
            writes.push((1, plan));
            Ok(())
        })
        .unwrap();
        keys.apply(0x42, KeyState::Down, changed, false, |plan| {
            writes.push((1, plan));
            Ok(())
        })
        .unwrap();
        assert_eq!(keys.held(0x42), Some(first));
        assert_eq!(keys.len(), 2);
        keys.apply(0x42, KeyState::Repeat, changed, false, |plan| {
            writes.push((2, plan));
            Ok(())
        })
        .unwrap();
        keys.apply(0x41, KeyState::Up, changed, false, |plan| {
            writes.push((0, plan));
            Ok(())
        })
        .unwrap();
        assert!(keys.held(0x41).is_none());
        assert!(keys.held(0x42).is_some());
        keys.apply(0x42, KeyState::Up, changed, false, |plan| {
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
            modifier: None,
        };
        assert!(
            keys.apply(0x41, KeyState::Down, first, false, |_| Err(
                InputError::new("injection_failed", "possible down")
            ))
            .is_err()
        );
        assert_eq!(keys.held(0x41), Some(first));
        let changed = OwnedKeyPlan {
            target: 48,
            native: 48,
            modifier: None,
        };
        assert!(
            keys.apply(0x41, KeyState::Up, changed, false, |plan| {
                assert_eq!(plan, 30);
                Err(InputError::new("release_failed", "possible up"))
            })
            .is_err()
        );
        assert_eq!(keys.held(0x41), Some(first));
        keys.apply(0x41, KeyState::Up, changed, false, |plan| {
            assert_eq!(plan, 30);
            Ok(())
        })
        .unwrap();
        assert!(keys.sources.is_empty());
    }

    #[test]
    fn only_marked_temporary_modifiers_share_related_native_sides_and_keep_first_plan() {
        for group in [
            HostModifier::Shift,
            HostModifier::Control,
            HostModifier::Alt,
        ] {
            let mut keys = HeldKeyPlans::new();
            let right = OwnedKeyPlan {
                target: 97,
                native: "first right plan",
                modifier: Some(group),
            };
            let left = OwnedKeyPlan {
                target: 29,
                native: "opposite left plan",
                modifier: Some(group),
            };
            let changed = OwnedKeyPlan {
                target: 30,
                native: "changed nonmodifier plan",
                modifier: None,
            };
            let mut writes = Vec::new();
            keys.apply(0x41, KeyState::Down, right, false, |plan| {
                writes.push(plan);
                Ok(())
            })
            .unwrap();
            assert_eq!(keys.select(0x11, left, false), left);
            assert_eq!(keys.select(0x11, left, true), right);
            keys.apply(0x11, KeyState::Down, left, true, |_| {
                panic!("temporary modifier must reuse the owned side")
            })
            .unwrap();
            assert_eq!(keys.held(0x11), Some(right));
            keys.apply(0xa2, KeyState::Down, left, false, |plan| {
                writes.push(plan);
                Ok(())
            })
            .unwrap();
            assert_eq!(keys.held(0xa2), Some(left));
            keys.apply(0xa2, KeyState::Up, left, false, |plan| {
                writes.push(plan);
                Ok(())
            })
            .unwrap();
            keys.apply(0x41, KeyState::Up, changed, false, |_| {
                panic!("temporary source still owns the first plan")
            })
            .unwrap();
            assert!(!keys.contains_key(&0x41));
            assert!(
                keys.apply(0x11, KeyState::Up, changed, true, |plan| {
                    assert_eq!(plan, right.native);
                    Err(InputError::new(
                        "release_failed",
                        "ambiguous final temporary release",
                    ))
                })
                .is_err()
            );
            assert_eq!(keys.held(0x11), Some(right));
            keys.apply(0x11, KeyState::Up, changed, true, |plan| {
                writes.push(plan);
                Ok(())
            })
            .unwrap();
            assert!(keys.is_empty());
            assert_eq!(
                writes,
                [
                    "first right plan",
                    "opposite left plan",
                    "opposite left plan",
                    "first right plan"
                ]
            );
        }
    }
}
