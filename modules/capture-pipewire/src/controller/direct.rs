//! Named KWin outputs have independent sources and leases; portals stay separate.
use super::{Created, Reader, Reply};
use crate::{
    admission::Admission,
    backend::{Backend, SourceSession},
    model::{CaptureResult, SourceState, new_id, validate_operation_id},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
struct Source {
    owner: String,
    operation: String,
    output: String,
    session: Box<dyn SourceSession>,
    admission: Arc<Admission>,
    id: Option<u64>,
    started: u64,
}
#[derive(Default)]
pub(super) struct DirectSources {
    sources: BTreeMap<(String, String), Source>,
    retiring: BTreeMap<(String, String), Source>,
    closed: BTreeMap<u64, String>,
    opened: u64,
}
impl DirectSources {
    pub fn open(
        &mut self,
        backend: &impl Backend,
        caller: &str,
        output: String,
        operation: Option<String>,
        now: u64,
        timeout: u32,
    ) -> CaptureResult<Reply> {
        let Some(name) = output
            .strip_prefix("kwin:")
            .filter(|name| valid_direct_name(name, true))
        else {
            return Err("invalid_named_output");
        };
        let operation = operation.ok_or("operation_id_required")?;
        validate_operation_id(&operation)?;
        let key = (caller.to_owned(), operation.clone());
        if let Some(existing) = self.sources.get(&key) {
            if existing.output != output {
                return Err("operation_conflict");
            }
        } else {
            if self.retiring.contains_key(&key) {
                return Err("source_retirement_pending");
            }
            if self.sources.len() + self.retiring.len() >= 4 || self.opened >= 128 {
                return Err("resource_exhausted");
            }
            let source = backend.select_output(name, timeout)?;
            self.sources.insert(
                key.clone(),
                Source {
                    owner: caller.into(),
                    operation,
                    output,
                    session: source,
                    admission: Arc::new(Admission::default()),
                    id: None,
                    started: now,
                },
            );
        }
        let source = self.sources.get_mut(&key).unwrap();
        match source.session.state() {
            SourceState::Selecting => Err("named_source_pending"),
            SourceState::Ready { .. } => {
                let created = source.id.is_none();
                if created {
                    source.id = Some(new_id()?);
                    self.opened += 1;
                }
                let id = source.id.unwrap();
                Ok(Reply {
                    payload: response(source),
                    created: created.then_some(Created::Lease(id)),
                })
            }
            _ => {
                self.retire(&key);
                Err("named_source_unavailable")
            }
        }
    }
    pub fn lookup(&self, caller: &str, operation: &str) -> Option<Value> {
        self.sources
            .get(&(caller.into(), operation.into()))
            .filter(|source| source.id.is_some())
            .map(response)
    }
    pub fn reader(&self, caller: &str, id: u64) -> Option<CaptureResult<Reader>> {
        let source = self
            .sources
            .values()
            .find(|source| source.owner == caller && source.id == Some(id))?;
        Some(
            if matches!(source.session.state(), SourceState::Ready { .. }) {
                Ok(Reader {
                    mailbox: source.session.mailbox(),
                    admission: source.admission.clone(),
                    geometry: source.session.geometry(),
                })
            } else {
                Err("source_unavailable")
            },
        )
    }
    pub fn close(&mut self, caller: &str, id: u64) -> Option<CaptureResult<()>> {
        if self.closed.get(&id).is_some_and(|owner| owner == caller) {
            return Some(Ok(()));
        }
        if self
            .retiring
            .values()
            .any(|source| source.owner == caller && source.id == Some(id))
        {
            return Some(Err("source_retirement_pending"));
        }
        let key = self
            .sources
            .iter()
            .find(|(_, source)| source.owner == caller && source.id == Some(id))
            .map(|(key, _)| key.clone());
        if let Some(key) = key {
            self.retire(&key);
            self.poll_retired();
            Some(if self.closed.contains_key(&id) {
                Ok(())
            } else {
                Err("source_retirement_pending")
            })
        } else {
            None
        }
    }
    pub fn cancel_pending(&mut self, caller: &str, operation: &str) {
        let key = (caller.to_owned(), operation.to_owned());
        if self
            .sources
            .get(&key)
            .is_some_and(|source| source.id.is_none())
        {
            self.retire(&key);
        }
    }
    pub fn active_count(&self) -> usize {
        self.sources
            .values()
            .filter(|source| source.id.is_some())
            .count()
    }

    pub fn refresh(&mut self, now: u64, timeout: u32) {
        let expired: Vec<_> = self
            .sources
            .iter()
            .filter(|(_, source)| {
                source.id.is_none()
                    && now.saturating_sub(source.started) >= u64::from(timeout) * 1000
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            self.retire(&key);
        }
        self.poll_retired();
    }
    pub fn streams(&self, caller: &str) -> Vec<Value> {
        self.sources
            .values()
            .filter(|source| source.owner == caller && source.id.is_some())
            .map(response)
            .collect()
    }
    pub fn shutdown(&mut self) {
        let keys: Vec<_> = self.sources.keys().cloned().collect();
        for key in keys {
            self.retire(&key);
        }
        self.poll_retired();
    }

    pub fn retired(&self) -> bool {
        self.retiring.is_empty()
    }

    fn retire(&mut self, key: &(String, String)) {
        if let Some(mut source) = self.sources.remove(key) {
            source.admission.close();
            source.session.begin_stop();
            self.retiring.insert(key.clone(), source);
            self.poll_retired();
        }
    }

    fn poll_retired(&mut self) {
        let finished: Vec<_> = self
            .retiring
            .iter()
            .filter(|(_, source)| source.session.stopped() && source.admission.retired())
            .map(|(key, _)| key.clone())
            .collect();
        for key in finished {
            if let Some(source) = self.retiring.remove(&key) {
                if let Some(id) = source.id {
                    self.closed.insert(id, source.owner.clone());
                }
                drop(source);
            }
        }
    }
}
fn response(source: &Source) -> Value {
    let mut response = json!({"ok":true,"stream":source.id,"output":source.output,"operation_id":source.operation,"geometry_supported":false});
    if let Some(geometry) = source.session.geometry() {
        response["desktop_geometry"] = json!(geometry);
        response["geometry_supported"] = json!(true);
    }
    if matches!(source.session.state(), SourceState::Ready { .. })
        && let Some(observed) = source.session.observed_capture()
    {
        response["observed_capture"] = json!(observed);
    }
    response
}

pub(super) fn valid_name(name: &str) -> bool {
    name.strip_prefix("Virtual-")
        .filter(|number| !number.starts_with('0') && number.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|number| number.parse::<u32>().ok())
        .is_some_and(|number| number > 0)
}

pub(super) fn valid_direct_name(name: &str, allow_desktop_outputs: bool) -> bool {
    valid_name(name)
        || (allow_desktop_outputs
            && !name.starts_with("Virtual-")
            && !name.is_empty()
            && name.len() <= 127
            && !name.chars().any(char::is_control))
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    #[test]
    fn named_output_accepts_bounded_positive_indices() {
        for name in ["Virtual-1", "Virtual-9", "Virtual-4294967295"] {
            assert!(valid_name(name));
        }
        for name in [
            "Virtual-0",
            "Virtual-01",
            "Virtual-",
            "Virtual--1",
            "Virtual-4294967296",
            "Virtual-1x",
        ] {
            assert!(!valid_name(name));
        }
    }
}
