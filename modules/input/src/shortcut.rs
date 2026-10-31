//! Keyboard shortcut matching with pinned press ownership.
//!
//! This state machine does not authorize actions or perform effects. The caller
//! must stage a whole input batch against a cloned matcher, durably admit all
//! intents together with the input receipt, and only then publish the clone.
use crate::model::{Key, KeyState};
use serde_json::Value;

const MAX_BINDINGS: usize = 128;
const MAX_CHORD_KEYS: usize = 8;
const MAX_HELD_KEYS: usize = 128;

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub trigger: Key,
    pub modifiers: Vec<Key>,
    pub action_id: String,
    pub arguments: Value,
    pub consume: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Intent {
    pub action_id: String,
    pub arguments: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Forward,
    Consume,
}

#[derive(Clone)]
struct HeldKey {
    key: Key,
    disposition: Disposition,
}

/// Holds source-key ownership independently from mutable binding configuration.
#[derive(Clone, Default)]
pub struct Matcher {
    bindings: Vec<Binding>,
    held: Vec<HeldKey>,
}

impl Matcher {
    /// Replace bindings only after validating the entire proposed set.
    pub fn replace_bindings(&mut self, bindings: Vec<Binding>) -> Result<(), &'static str> {
        if bindings.len() > MAX_BINDINGS {
            return Err("too many shortcut bindings");
        }
        for (index, binding) in bindings.iter().enumerate() {
            if binding.action_id.is_empty() || binding.modifiers.len() > MAX_CHORD_KEYS {
                return Err("invalid shortcut binding");
            }
            if binding
                .modifiers
                .iter()
                .any(|key| same_key(key, &binding.trigger))
                || binding.modifiers.iter().enumerate().any(|(i, key)| {
                    binding.modifiers[..i]
                        .iter()
                        .any(|other| same_key(key, other))
                })
            {
                return Err("duplicate key in shortcut chord");
            }
            if bindings[..index]
                .iter()
                .any(|other| same_key(&binding.trigger, &other.trigger))
            {
                return Err("ambiguous shortcut chord");
            }
        }
        self.bindings = bindings;
        Ok(())
    }

    /// Stage one edge on a cloned matcher. `admit` records a provisional intent
    /// in the caller-owned batch plan; it must perform no external side effects.
    /// The caller publishes the clone only after durable admission of the whole
    /// batch and its input receipt. Failure leaves this edge unchanged.
    pub fn edge<E>(
        &mut self,
        key: &Key,
        state: KeyState,
        mut admit: impl FnMut(&Intent, Disposition) -> Result<(), E>,
    ) -> Result<Disposition, EdgeError<E>> {
        // Temporary Moonlight modifier events are synthesized around another
        // key and cannot establish a physical shortcut chord.
        if matches!(
            key,
            Key::MoonlightVk {
                temporary_modifier: true,
                ..
            }
        ) {
            return Ok(Disposition::Forward);
        }
        let held = self.held.iter().position(|entry| same_key(&entry.key, key));
        match state {
            KeyState::Up => Ok(held.map_or(Disposition::Forward, |index| {
                self.held.remove(index).disposition
            })),
            KeyState::Repeat => {
                Ok(held.map_or(Disposition::Forward, |index| self.held[index].disposition))
            }
            KeyState::Down if held.is_some() => Ok(self.held[held.unwrap()].disposition),
            KeyState::Down => {
                if self.held.len() == MAX_HELD_KEYS {
                    return Err(EdgeError::Capacity);
                }
                let matched = self.bindings.iter().find(|binding| {
                    same_key(&binding.trigger, key)
                        && binding.modifiers.iter().all(|modifier| {
                            self.held.iter().any(|held| same_key(&held.key, modifier))
                        })
                });
                let disposition = if let Some(binding) = matched {
                    let disposition = if binding.consume {
                        Disposition::Consume
                    } else {
                        Disposition::Forward
                    };
                    admit(
                        &Intent {
                            action_id: binding.action_id.clone(),
                            arguments: binding.arguments.clone(),
                        },
                        disposition,
                    )
                    .map_err(EdgeError::Admission)?;
                    disposition
                } else {
                    Disposition::Forward
                };
                self.held.push(HeldKey {
                    key: key.clone(),
                    disposition,
                });
                Ok(disposition)
            }
        }
    }

    /// Restore persisted source ownership without firing shortcuts.
    pub fn recover_held(&mut self, keys: &[(Key, Disposition)]) -> Result<(), &'static str> {
        if keys.len() > MAX_HELD_KEYS
            || keys.iter().enumerate().any(|(index, (key, _))| {
                keys[..index].iter().any(|(other, _)| same_key(key, other))
            })
        {
            return Err("invalid held key snapshot");
        }
        self.held = keys
            .iter()
            .cloned()
            .map(|(key, disposition)| HeldKey { key, disposition })
            .collect();
        Ok(())
    }

    /// Retire held ownership on lease reset; no synthetic shortcut is fired.
    pub fn reset(&mut self) {
        self.held.clear();
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EdgeError<E> {
    Capacity,
    Admission(E),
}

fn same_key(a: &Key, b: &Key) -> bool {
    match (a, b) {
        (
            Key::WindowsScan {
                code: a,
                extended: ae,
            },
            Key::WindowsScan {
                code: b,
                extended: be,
            },
        ) => a == b && ae == be,
        (Key::LinuxEvdev { code: a }, Key::LinuxEvdev { code: b }) => a == b,
        (
            Key::MoonlightVk {
                code: a,
                normalized: an,
                ..
            },
            Key::MoonlightVk {
                code: b,
                normalized: bn,
                ..
            },
        ) => a == b && an == bn,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(code: u16) -> Key {
        Key::LinuxEvdev { code }
    }
    fn binding() -> Binding {
        Binding {
            trigger: key(2),
            modifiers: vec![key(1)],
            action_id: "test/remove".into(),
            arguments: json!({}),
            consume: true,
        }
    }
    fn edge(m: &mut Matcher, key: u16, state: KeyState) -> Disposition {
        m.edge(&self::key(key), state, |_, _| Ok::<_, ()>(()))
            .unwrap()
    }

    #[test]
    fn admission_failure_preserves_state_and_consumed_release_is_pinned() {
        let mut matcher = Matcher::default();
        matcher.replace_bindings(vec![binding()]).unwrap();
        assert_eq!(edge(&mut matcher, 1, KeyState::Down), Disposition::Forward);
        assert_eq!(
            matcher.edge(&key(2), KeyState::Down, |_, _| Err("full")),
            Err(EdgeError::Admission("full"))
        );
        let mut admitted = 0;
        assert_eq!(
            matcher
                .edge(&key(2), KeyState::Down, |_, _| {
                    admitted += 1;
                    Ok::<_, ()>(())
                })
                .unwrap(),
            Disposition::Consume
        );
        matcher.replace_bindings(vec![]).unwrap();
        assert_eq!(
            edge(&mut matcher, 2, KeyState::Repeat),
            Disposition::Consume
        );
        assert_eq!(edge(&mut matcher, 2, KeyState::Down), Disposition::Consume);
        assert_eq!(edge(&mut matcher, 2, KeyState::Up), Disposition::Consume);
        assert_eq!(edge(&mut matcher, 1, KeyState::Up), Disposition::Forward);
        assert_eq!(admitted, 1);
    }

    #[test]
    fn recovered_snapshot_does_not_create_an_action() {
        let mut matcher = Matcher::default();
        matcher.replace_bindings(vec![binding()]).unwrap();
        matcher
            .recover_held(&[
                (key(1), Disposition::Forward),
                (key(2), Disposition::Consume),
            ])
            .unwrap();
        assert_eq!(edge(&mut matcher, 2, KeyState::Down), Disposition::Consume);
        assert_eq!(edge(&mut matcher, 2, KeyState::Up), Disposition::Consume);
    }

    #[test]
    fn conflicting_chords_rejected_atomically_and_reset_does_not_trigger() {
        let mut matcher = Matcher::default();
        matcher.replace_bindings(vec![binding()]).unwrap();
        assert!(
            matcher
                .replace_bindings(vec![binding(), binding()])
                .is_err()
        );
        edge(&mut matcher, 1, KeyState::Down);
        matcher.reset();
        assert_eq!(edge(&mut matcher, 2, KeyState::Down), Disposition::Forward);
    }
}
