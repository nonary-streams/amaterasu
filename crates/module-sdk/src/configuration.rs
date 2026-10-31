//! Product-owned settings operations for `amaterasu.configuration/1`.
//!
//! The broker supplies the caller identity; there is no owner selector. Product
//! updates replace the full document using compare-and-swap, and a cancelled or
//! lost response does not prove the write was undone. Generic schema operations
//! remain available through the ordinary broker protocol.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const CONFIGURATION_SERVICE: &str = "amaterasu.configuration/1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProductSettingsRequest {
    ProductGet {},
    ProductUpdate {
        version: u32,
        values: Map<String, Value>,
        expected_revision: u64,
    },
}

impl ProductSettingsRequest {
    /// Submit using the broker's authenticated caller and explicitly scoped parent.
    /// Admission only queues work; poll for the terminal result and release it.
    pub fn submit(
        &self,
        runtime: &crate::RuntimeClient,
        timeout_ms: u64,
        operation_id: &str,
        parent: Option<u64>,
    ) -> crate::ModuleResult<u64> {
        runtime.request_serialized(
            crate::requests::RequestOptions {
                service: CONFIGURATION_SERVICE,
                timeout_ms,
                operation_id,
                parent,
            },
            self,
        )
    }

    pub fn payload(&self) -> Value {
        serde_json::to_value(self).expect("settings request contains only JSON values")
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct SettingsError {
    pub code: String,
    pub message: String,
    pub current_revision: Option<u64>,
}

/// A product document has version null and revision zero until first saved.
#[derive(Debug, Deserialize)]
pub struct ProductSettingsDocument {
    pub found: bool,
    pub version: Option<u32>,
    pub revision: u64,
    pub values: Map<String, Value>,
}

pub fn decode_response(
    payload: Value,
) -> Result<Result<ProductSettingsDocument, SettingsError>, String> {
    match payload.get("ok").and_then(Value::as_bool) {
        Some(true) => {
            if payload.get("version").is_none() {
                return Err("missing product settings version".into());
            }
            let document: ProductSettingsDocument =
                serde_json::from_value(payload).map_err(|error| error.to_string())?;
            if (document.found
                && (document.version.is_none_or(|version| version == 0) || document.revision == 0))
                || (!document.found
                    && (document.version.is_some()
                        || document.revision != 0
                        || !document.values.is_empty()))
            {
                return Err("inconsistent product settings document".into());
            }
            Ok(Ok(document))
        }
        Some(false) => serde_json::from_value(
            payload
                .get("error")
                .cloned()
                .ok_or("missing settings error")?,
        )
        .map(Err)
        .map_err(|error| error.to_string()),
        None => Err("missing settings response status".into()),
    }
}
