//! Keyboard shortcut matching with pinned press ownership.
//!
//! This state machine does not authorize actions or perform effects. The caller
//! must stage a whole input batch against a cloned matcher, durably admit all
//! intents together with the input receipt, and only then publish the clone.
use crate::model::{Event, Key, KeyState};
use amaterasu_extension_api::{
    BindingPropagation, BindingSource, BindingTarget, KeyIdentity, Keybinding,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_BINDINGS: usize = 128;
const MAX_CHORD_KEYS: usize = 8;
const MAX_HELD_KEYS: usize = 128;

#[derive(Clone, Debug, PartialEq)]
struct Binding {
    pub binding_id: String,
    pub target: BindingTarget,
    pub trigger: Key,
    pub modifiers: Vec<Key>,
    pub action_id: String,
    pub arguments: Value,
    pub consume: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Intent {
    pub binding_id: String,
    pub target: BindingTarget,
    pub action_id: String,
    pub arguments: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    /// Select already-approved bindings for one source/context. This does not
    /// grant authority: the caller must authenticate the source and validate
    /// the action descriptor and arguments before supplying these bindings.
    pub fn replace_approved_bindings(
        &mut self,
        bindings: &[Keybinding],
        source: BindingSource,
        context: &str,
    ) -> Result<(), &'static str> {
        let mut selected = Vec::new();
        for binding in bindings.iter().filter(|binding| {
            binding.enabled && binding.source == source && binding.context == context
        }) {
            if source == BindingSource::SessionInput && binding.target == BindingTarget::Host {
                return Err("remote input cannot target host actions");
            }
            let trigger = key_identity(&binding.trigger);
            let modifiers: Vec<Key> = binding.modifiers.iter().map(key_identity).collect();
            if std::iter::once(&trigger).chain(&modifiers).any(|key| {
                Event::Key {
                    key: key.clone(),
                    state: KeyState::Down,
                }
                .validate()
                .is_err()
            }) || modifiers
                .iter()
                .any(|key| key.encoding() != trigger.encoding())
            {
                return Err("unsupported shortcut key identity");
            }
            selected.push(Binding {
                binding_id: binding.id.clone(),
                target: binding.target.clone(),
                trigger,
                modifiers,
                action_id: binding.action_id.clone(),
                arguments: serde_json::to_value(&binding.arguments)
                    .map_err(|_| "invalid action arguments")?,
                consume: binding.propagation == BindingPropagation::ConsumeTrigger,
            });
        }
        self.replace_bindings(selected)
    }

    /// Replace bindings only after validating the entire proposed set.
    fn replace_bindings(&mut self, bindings: Vec<Binding>) -> Result<(), &'static str> {
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
                            binding_id: binding.binding_id.clone(),
                            target: binding.target.clone(),
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

    /// Return the exact ownership state to persist with a staged receipt.
    pub fn held_snapshot(&self) -> Vec<(Key, Disposition)> {
        self.held
            .iter()
            .map(|entry| (entry.key.clone(), entry.disposition))
            .collect()
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

fn key_identity(identity: &KeyIdentity) -> Key {
    match identity {
        KeyIdentity::LinuxEvdev { code } => Key::LinuxEvdev { code: *code },
        KeyIdentity::WindowsScan { code, extended } => Key::WindowsScan {
            code: *code,
            extended: *extended,
        },
        KeyIdentity::MoonlightVk { code, normalized } => Key::MoonlightVk {
            code: *code,
            normalized: *normalized,
            repeat_modifiers: 0,
            temporary_modifier: false,
        },
    }
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
            binding_id: "binding".into(),
            target: BindingTarget::CurrentSession,
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
    fn approved_session_binding_rejects_host_scope_and_preserves_intent_identity() {
        let mut matcher = Matcher::default();
        let mut binding = Keybinding {
            id: "remove-binding".into(),
            enabled: true,
            source: BindingSource::SessionInput,
            context: "game".into(),
            trigger: KeyIdentity::LinuxEvdev { code: 2 },
            modifiers: vec![KeyIdentity::LinuxEvdev { code: 1 }],
            propagation: BindingPropagation::ConsumeTrigger,
            action_id: "test/remove".into(),
            arguments: Default::default(),
            target: BindingTarget::Host,
        };
        assert!(
            matcher
                .replace_approved_bindings(&[binding.clone()], BindingSource::SessionInput, "game")
                .is_err()
        );
        binding.target = BindingTarget::CurrentSession;
        matcher
            .replace_approved_bindings(&[binding], BindingSource::SessionInput, "game")
            .unwrap();
        edge(&mut matcher, 1, KeyState::Down);
        let mut intent = None;
        matcher
            .edge(&key(2), KeyState::Down, |value, _| {
                intent = Some(value.clone());
                Ok::<_, ()>(())
            })
            .unwrap();
        let intent = intent.unwrap();
        assert_eq!(intent.binding_id, "remove-binding");
        assert_eq!(intent.target, BindingTarget::CurrentSession);
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
    fn forwarded_held_modifier_can_start_a_newly_enabled_shortcut() {
        let mut matcher = Matcher::default();
        matcher.replace_bindings(vec![binding()]).unwrap();
        // The native fast path forwarded this modifier before shortcuts were
        // enabled; recovery must treat it as held without replaying its edge.
        matcher
            .recover_held(&[(key(1), Disposition::Forward)])
            .unwrap();
        let mut actions = 0;
        assert_eq!(
            matcher
                .edge(&key(2), KeyState::Down, |_, _| {
                    actions += 1;
                    Ok::<_, ()>(())
                })
                .unwrap(),
            Disposition::Consume
        );
        assert_eq!(actions, 1);
        assert_eq!(edge(&mut matcher, 2, KeyState::Up), Disposition::Consume);
        assert_eq!(edge(&mut matcher, 1, KeyState::Up), Disposition::Forward);
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
