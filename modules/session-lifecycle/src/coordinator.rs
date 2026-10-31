//! Durable admission and replay protection for lifecycle transitions.
use crate::store::LifecycleStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io, path::Path};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    caller: String,
    initiator: String,
    request: Value,
    response: Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    host_epoch: String,
    receipts: BTreeMap<String, Receipt>,
}

// JSON array encoding is unambiguous for arbitrary authenticated identity strings.
fn receipt_key(caller: &str, initiator: &str, operation_id: &str) -> String {
    serde_json::to_string(&(caller, initiator, operation_id)).expect("string tuple serializes")
}

fn validate_journal(journal: &Journal) -> io::Result<()> {
    let valid_text = |value: Option<&str>| value.is_some_and(|text| !text.is_empty());
    let valid = !journal.host_epoch.is_empty()
        && journal.receipts.len() <= 4096
        && journal.receipts.iter().all(|(key, receipt)| {
            let operation_id = receipt.request.get("operation_id").and_then(Value::as_str);
            let response = &receipt.response;
            let status = response.get("status").and_then(Value::as_str);
            !receipt.caller.is_empty()
                && !receipt.initiator.is_empty()
                && operation_id.is_some_and(|id| {
                    !id.is_empty()
                        && id.len() <= 128
                        && *key == receipt_key(&receipt.caller, &receipt.initiator, id)
                })
                && receipt.request.get("operation").and_then(Value::as_str) == Some("prepare")
                && response.get("operation_id").and_then(Value::as_str) == operation_id
                && valid_text(response.get("host_epoch").and_then(Value::as_str))
                && valid_text(response.get("session_id").and_then(Value::as_str))
                && valid_text(response.get("transition_id").and_then(Value::as_str))
                && response.get("generation").and_then(Value::as_u64) == Some(1)
                && (status == Some("pending")
                    || (status == Some("recovery_required")
                        && response.get("reason").and_then(Value::as_str)
                            == Some("coordinator_restarted")))
        });
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid lifecycle journal",
        ))
    }
}

pub struct Coordinator {
    store: LifecycleStore,
    journal: Journal,
}

impl Coordinator {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut store = LifecycleStore::new(path.to_path_buf())?;
        let previous: Option<Journal> = store.load()?;
        let epoch = amaterasu_storage::random_identifier().map_err(io::Error::other)?;
        let mut journal = match previous {
            Some(journal) if journal.version == 2 => journal,
            Some(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unsupported lifecycle journal version",
                ));
            }
            None => Journal {
                version: 2,
                host_epoch: epoch.clone(),
                receipts: BTreeMap::new(),
            },
        };
        validate_journal(&journal)?;
        // In-flight operations from the old process require reconciliation; never
        // replay their native work merely because the process was restarted.
        for receipt in journal.receipts.values_mut() {
            if receipt.response.get("status").and_then(Value::as_str) == Some("pending") {
                receipt.response["status"] = json!("recovery_required");
                receipt.response["reason"] = json!("coordinator_restarted");
            }
        }
        journal.host_epoch = epoch;
        store.replace(&journal)?;
        Ok(Self { store, journal })
    }

    pub fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value {
        if caller.is_empty() || initiator.is_empty() {
            return json!({"status":"rejected","reason":"missing_authenticated_identity"});
        }
        let Some(operation) = payload.get("operation").and_then(Value::as_str) else {
            return json!({"status":"rejected","reason":"invalid_request"});
        };
        if operation == "snapshot" {
            return json!({"status":"ok","host_epoch":self.journal.host_epoch});
        }
        let Some(operation_id) = payload
            .get("operation_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 128)
        else {
            return json!({"status":"rejected","reason":"invalid_operation_id"});
        };
        let receipt_key = receipt_key(caller, initiator, operation_id);
        if operation == "lookup" {
            return match self.journal.receipts.get(&receipt_key) {
                Some(receipt) if receipt.caller == caller && receipt.initiator == initiator => {
                    receipt.response.clone()
                }
                _ => json!({"status":"not_found"}),
            };
        }
        if let Some(receipt) = self.journal.receipts.get(&receipt_key) {
            return if receipt.caller == caller
                && receipt.initiator == initiator
                && receipt.request == payload
            {
                receipt.response.clone()
            } else {
                json!({"status":"rejected","reason":"operation_id_conflict"})
            };
        }
        if operation != "prepare" {
            return json!({"status":"rejected","reason":"unsupported_operation"});
        }
        if self.journal.receipts.len() >= 4096 {
            return json!({"status":"rejected","reason":"capacity_exhausted"});
        }
        // This stage only admits preparation; it does not assert that providers
        // prepared resources or that an application may launch.
        let session_id = match amaterasu_storage::random_identifier() {
            Ok(id) => id,
            Err(_) => return json!({"status":"rejected","reason":"entropy_unavailable"}),
        };
        let transition_id = match amaterasu_storage::random_identifier() {
            Ok(id) => id,
            Err(_) => return json!({"status":"rejected","reason":"entropy_unavailable"}),
        };
        let response = json!({"status":"pending","operation_id":operation_id,"host_epoch":self.journal.host_epoch,"session_id":session_id,"transition_id":transition_id,"generation":1});
        let mut next = self.journal.clone();
        next.receipts.insert(
            receipt_key,
            Receipt {
                caller: caller.into(),
                initiator: initiator.into(),
                request: payload.clone(),
                response: response.clone(),
            },
        );
        if self.store.replace(&next).is_err() {
            return json!({"status":"recovery_required","reason":"journal_write_failed"});
        }
        self.journal = next;
        response
    }
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
