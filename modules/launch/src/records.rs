use crate::protocol::{
    ApplicationConfigOverrides, ApplicationDisplaySettings, LaunchTarget, LegacyPreparationCommand,
    Outcome, PreparationStep, State,
};
use amaterasu_contracts::virtual_display::VirtualDisplayIntent;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CatalogSelection {
    Initiator,
    Exported,
}

#[derive(Clone, Default, PartialEq)]
pub struct GlobalPreparation {
    pub structured_steps: Vec<PreparationStep>,
    pub legacy_commands: Vec<LegacyPreparationCommand>,
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
    /// Conservative cleanup pass budget frozen before any child or lease effect.
    pub cleanup_budget_ms: u64,
    #[serde(skip)]
    pub owner: String,
    #[serde(skip)]
    pub catalog_principal: Option<String>,
    #[serde(skip)]
    pub catalog_selection: CatalogSelection,
    #[serde(skip)]
    pub lutris_source_id: Option<i64>,
    #[serde(skip)]
    pub catalog_initiator: String,
    #[serde(skip)]
    pub session_key: String,
    #[serde(skip)]
    pub process_operation: String,
    #[serde(skip)]
    pub preparation_operation: String,
    #[serde(skip)]
    pub resource: Option<String>,
    #[serde(skip)]
    pub target: Option<LaunchTarget>,
    #[serde(skip)]
    pub output: Option<String>,
    /// Launch-time global preparation snapshot retained for idempotency and retries.
    #[serde(skip)]
    pub global_preparation: Vec<PreparationStep>,
    #[serde(skip)]
    pub legacy_global_preparation: Vec<LegacyPreparationCommand>,
    #[serde(skip)]
    pub provider_ticket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_overrides: Option<ApplicationConfigOverrides>,
    /// Sparse per-application output/virtual-screen controls frozen from the
    /// catalog row. They are returned during preflight but kept off status.
    #[serde(skip)]
    pub application_display_settings: Option<ApplicationDisplaySettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_limiter_settings: Option<BTreeMap<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_frame_limiter_overrides: Option<BTreeMap<String, Value>>,
    pub application_display_overrides: BTreeMap<String, Value>,
    #[serde(skip)]
    pub display_admission: Option<DisplayAdmission>,
    /// Resolved request is private runtime state, not a public launch-status field.
    #[serde(skip)]
    pub virtual_display_intent: Option<VirtualDisplayIntent>,
    #[serde(skip)]
    pub proton_dualsense_compatibility: bool,
    #[serde(skip)]
    pub return_on_admission: bool,
    #[serde(skip)]
    pub preflight_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog_revision: Option<u64>,
    #[serde(skip)]
    pub preflight_ready: bool,
    #[serde(skip)]
    pub preflight_deadline: Option<Instant>,
    #[serde(skip)]
    pub launch_started: bool,
    #[serde(skip)]
    pub session_requested: bool,
    #[serde(skip)]
    pub process_requested: bool,
    #[serde(skip)]
    pub preparation_requested: bool,
    #[serde(skip)]
    pub preparation_stopped: bool,
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

pub struct DisplayAdmission {
    pub token: String,
    pub launch_token_required: bool,
    pub certificate_id: String,
    pub rtsp_operation_id: String,
    pub request_sha256: String,
    pub snapshot: Value,
}

pub fn random_display_admission_token() -> Result<String, &'static str> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
    Ok(hex(&bytes))
}
impl Record {
    pub fn response(&self) -> Value {
        json!({"ok":true,"launch":self})
    }
    pub fn preflight_response(&self) -> Value {
        json!({"ok":true,"preflight_token":self.preflight_token,"app_id":self.app_id,"catalog_revision":self.catalog_revision,"config_overrides":self.config_overrides.clone().unwrap_or_default(),"application_display_overrides":self.application_display_overrides,"application_display_settings":self.application_display_settings,"executable_path":self.executable_path,"cleanup_budget_ms":self.cleanup_budget_ms})
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
        global_preparation: GlobalPreparation,
    ) -> Result<(u64, bool), &'static str> {
        self.values.retain(|_, record| {
            record.launch_started
                || record.preflight_token.is_none()
                || (record.state == State::Starting
                    && (!record.preflight_ready
                        || record
                            .preflight_deadline
                            .is_some_and(|deadline| Instant::now() < deadline)))
        });
        let GlobalPreparation {
            structured_steps: global_preparation,
            legacy_commands: legacy_global_preparation,
        } = global_preparation;
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
            return if record.app_id == app_id
                && same_catalog
                && record.global_preparation == global_preparation
                && record.legacy_global_preparation == legacy_global_preparation
            {
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
                cleanup_budget_ms: 20_000,
                owner: caller.into(),
                catalog_principal,
                catalog_selection,
                lutris_source_id: None,
                catalog_initiator: initiator.into(),
                session_key: key.clone(),
                process_operation: key.clone(),
                preparation_operation: format!("{key}.preparation"),
                resource,
                target: None,
                output: None,
                global_preparation,
                legacy_global_preparation,
                provider_ticket: None,
                config_overrides: None,
                application_display_settings: None,
                executable_path: None,
                frame_limiter_settings: None,
                client_frame_limiter_overrides: None,
                application_display_overrides: BTreeMap::new(),
                display_admission: None,
                virtual_display_intent: None,
                proton_dualsense_compatibility: false,
                return_on_admission: false,
                preflight_token: None,
                catalog_revision: None,
                preflight_ready: false,
                preflight_deadline: None,
                launch_started: false,
                session_requested: false,
                process_requested: false,
                preparation_requested: false,
                preparation_stopped: false,
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
            .skip(offset)
            .peekable();
        let mut records = Vec::new();
        let mut byte_limit_reached = false;
        for record in owned.by_ref().take(limit) {
            records.push(record);
            let candidate = json!({"ok":true,"launches":records,"next_offset":usize::MAX});
            if !serde_json::to_vec(&candidate).is_ok_and(|bytes| bytes.len() <= 16_384) {
                records.pop();
                byte_limit_reached = true;
                break;
            }
        }
        if byte_limit_reached && records.is_empty() {
            return json!({"ok":false,"code":"launch_response_too_large"});
        }
        let next = if byte_limit_reached || owned.peek().is_some() {
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
