//! Durable intent for changing an owned target without replacing its allocation.
use crate::model::{Specification, valid_reference_id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Change {
    pub operation_id: String,
    pub reference_id: String,
    pub expected_reference_ids: BTreeSet<String>,
    pub expected_revision: u64,
    pub old: Specification,
    pub desired: Specification,
    pub completed: bool,
    #[serde(default)]
    pub aborted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(amaterasu_libvirtualdisplay)]
pub(crate) enum Step {
    Prepare,
    Commit,
    Abort,
}

impl Change {
    pub fn valid(&self, effective: &Specification) -> bool {
        !self.operation_id.is_empty()
            && self.expected_revision < u64::MAX
            && !(self.completed && self.aborted)
            && self.operation_id.len() <= 128
            && self
                .operation_id
                .bytes()
                .all(|byte| byte.is_ascii_graphic())
            && valid_reference_id(&self.reference_id)
            && !self.expected_reference_ids.is_empty()
            && self.expected_reference_ids.len() <= 64
            && self.expected_reference_ids.contains(&self.reference_id)
            && self
                .expected_reference_ids
                .iter()
                .all(|reference| valid_reference_id(reference))
            && self.old.validate().is_ok()
            && self.desired.validate().is_ok()
            && self.old.identity == self.desired.identity
            && self.old.name == self.desired.name
            && effective
                == if self.completed {
                    &self.desired
                } else {
                    &self.old
                }
    }

    pub fn references_match(&self, references: Option<&BTreeSet<String>>) -> bool {
        references == Some(&self.expected_reference_ids)
    }

    pub fn response(&self) -> Value {
        json!({"ok":true,"state":if self.completed {"succeeded"} else if self.aborted {"aborted"} else {"prepared"},
            "operation_id":self.operation_id,
            "reference_id":self.reference_id,
            "expected_reference_ids":self.expected_reference_ids,
            "expected_mode_revision":self.expected_revision,
            "specification":if self.aborted {&self.old} else {&self.desired},
            "mode_revision":self.mode_revision()})
    }

    pub fn mode_revision(&self) -> u64 {
        self.expected_revision
            .saturating_add(u64::from(self.completed || self.aborted))
    }
    pub fn is_pending(&self) -> bool {
        !self.completed && !self.aborted
    }
}

pub(crate) fn references(values: Vec<String>) -> Result<BTreeSet<String>, &'static str> {
    let count = values.len();
    if count == 0 || count > 64 || values.iter().any(|value| !valid_reference_id(value)) {
        return Err("invalid_request");
    }
    let values: BTreeSet<_> = values.into_iter().collect();
    if values.len() != count {
        return Err("invalid_request");
    }
    Ok(values)
}
