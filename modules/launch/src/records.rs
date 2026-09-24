use crate::protocol::{ApplicationConfigOverrides, LaunchTarget, Outcome, State};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CatalogSelection {
    Initiator,
    Exported,
}

#[derive(Serialize)]
pub struct Record {
    pub launch_id: u64,
    pub operation_id: String,
    pub app_id: String,
    pub state: State,
    pub session_id: Option<u64>,
    pub process_id: Option<u64>,
    pub failure: Option<&'static str>,
    pub cleanup_error: Option<&'static str>,
    #[serde(skip)]
    pub owner: String,
    #[serde(skip)]
    pub catalog_principal: Option<String>,
    #[serde(skip)]
    pub catalog_selection: CatalogSelection,
    #[serde(skip)]
    pub catalog_initiator: String,
    #[serde(skip)]
    pub session_key: String,
    #[serde(skip)]
    pub process_operation: String,
    #[serde(skip)]
    pub resource: Option<String>,
    #[serde(skip)]
    pub target: Option<LaunchTarget>,
    #[serde(skip)]
    pub provider_ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_overrides: Option<ApplicationConfigOverrides>,
    #[serde(skip)]
    pub session_requested: bool,
    #[serde(skip)]
    pub process_requested: bool,
    #[serde(skip)]
    pub provider_launch_requested: bool,
    #[serde(skip)]
    pub provider_stopped: bool,
    #[serde(skip)]
    pub cleanup_outcome: Outcome,
    // Retained until cancellation and broker release are confirmed.
    #[serde(skip)]
    pub outstanding_request: Option<u64>,
}
impl Record {
    pub fn response(&self) -> Value {
        json!({"ok":true,"launch":self})
    }
}
pub struct Records {
    pub values: BTreeMap<u64, Record>,
    nonce: String,
    next_id: u64,
}
impl Records {
    pub fn new() -> Result<Self, &'static str> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| "entropy_unavailable")?;
        Ok(Self {
            values: BTreeMap::new(),
            nonce: hex(&nonce),
            next_id: 0,
        })
    }
    pub fn start(
        &mut self,
        caller: &str,
        initiator: &str,
        operation_id: String,
        app_id: String,
        catalog_selection: CatalogSelection,
    ) -> Result<(u64, bool), &'static str> {
        if let Some(record) = self
            .values
            .values()
            .find(|record| record.owner == caller && record.operation_id == operation_id)
        {
            let same_catalog = match catalog_selection {
                CatalogSelection::Initiator => {
                    record.catalog_selection == CatalogSelection::Initiator
                        && record.catalog_principal.as_deref() == Some(initiator)
                }
                CatalogSelection::Exported => {
                    record.catalog_selection == CatalogSelection::Exported
                        && record.catalog_initiator == initiator
                }
            };
            return if record.app_id == app_id && same_catalog {
                Ok((record.launch_id, false))
            } else {
                Err("operation_conflict")
            };
        }
        if self.values.len() >= 32
            || self
                .values
                .values()
                .filter(|record| record.owner == caller)
                .count()
                >= 16
        {
            return Err("launch_limit");
        }
        let id = self.next_id.checked_add(1).ok_or("launch_limit")?;
        let key = format!("launch.{}.{}", self.nonce, id);
        let catalog_principal =
            (catalog_selection == CatalogSelection::Initiator).then(|| initiator.to_owned());
        let resource = catalog_principal
            .as_deref()
            .map(|owner| lease_resource(owner, &app_id));
        self.values.insert(
            id,
            Record {
                launch_id: id,
                operation_id,
                app_id,
                state: State::Starting,
                outstanding_request: None,
                session_id: None,
                process_id: None,
                failure: None,
                cleanup_error: None,
                owner: caller.into(),
                catalog_principal,
                catalog_selection,
                catalog_initiator: initiator.into(),
                session_key: key.clone(),
                process_operation: key,
                resource,
                target: None,
                provider_ticket: None,
                config_overrides: None,
                session_requested: false,
                process_requested: false,
                provider_launch_requested: false,
                provider_stopped: false,
                cleanup_outcome: Outcome::Completed,
            },
        );
        self.next_id = id;
        Ok((id, true))
    }
    pub fn owned(&self, caller: &str, id: u64) -> Result<&Record, &'static str> {
        self.values
            .get(&id)
            .filter(|record| record.owner == caller)
            .ok_or("launch_not_found")
    }
    pub fn owned_operation(
        &self,
        caller: &str,
        operation_id: &str,
    ) -> Result<&Record, &'static str> {
        self.values
            .values()
            .find(|record| record.owner == caller && record.operation_id == operation_id)
            .ok_or("launch_not_found")
    }
    pub fn list(&self, caller: &str, offset: usize, limit: usize) -> Value {
        let mut owned = self
            .values
            .values()
            .filter(|record| record.owner == caller)
            .skip(offset);
        let records: Vec<_> = owned.by_ref().take(limit).collect();
        let next = if owned.next().is_some() {
            offset.checked_add(records.len())
        } else {
            None
        };
        json!({"ok":true,"launches":records,"next_offset":next})
    }
}

impl Record {
    pub fn set_exported_catalog_owner(&mut self, owner: &str) -> Result<(), &'static str> {
        if self.catalog_selection != CatalogSelection::Exported {
            return Err("invalid_catalog_response");
        }
        if self
            .catalog_principal
            .as_deref()
            .is_some_and(|existing| existing != owner)
        {
            return Err("invalid_catalog_response");
        }
        self.catalog_principal = Some(owner.into());
        self.resource = Some(lease_resource(owner, &self.app_id));
        Ok(())
    }
}

fn lease_resource(owner: &str, app_id: &str) -> String {
    let digest = hex(&Sha256::digest(
        format!("{}:{owner}{app_id}", owner.len()).as_bytes(),
    ));
    format!("catalog-application/{digest}")
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
