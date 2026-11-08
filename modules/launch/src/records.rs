use crate::protocol::{
    ApplicationConfigOverrides, ApplicationDisplaySettings, LaunchTarget, LegacyPreparationCommand,
    Outcome, PreparationStep, State,
};
use amaterasu_contracts::virtual_display::VirtualDisplayIntent;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CatalogSelection {
    Initiator,
    Exported,
    Library,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_selection: Option<crate::library::Selection>,
    #[serde(skip)]
    pub preparation_context: Option<amaterasu_module_sdk::extension_api::LifecycleTarget>,
    #[serde(skip)]
    pub prepared_context_service: Option<String>,
    #[serde(skip)]
    pub prepared_environment: Option<BTreeMap<String, String>>,
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
    /// The preflight lifetime policy remains stable after the target is retired.
    #[serde(skip)]
    pub terminal_stop_on_stream_end: Option<bool>,
    #[serde(skip)]
    pub output: Option<String>,
    /// Launch-time global preparation snapshot retained for idempotency and retries.
    #[serde(skip)]
    pub global_preparation: Vec<PreparationStep>,
    #[serde(skip)]
    pub legacy_global_preparation: Vec<LegacyPreparationCommand>,
    #[serde(skip)]
    pub provider_ticket: Option<String>,
    #[serde(skip)]
    pub generic_provider_service: Option<String>,
    #[serde(skip)]
    pub generic_provider_incarnation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_overrides: Option<ApplicationConfigOverrides>,
    /// Sparse per-application output/virtual-screen controls frozen from the
    /// catalog row. They are returned during preflight but kept off status.
    #[serde(skip)]
    pub application_display_settings: Option<ApplicationDisplaySettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_path: Option<String>,
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
    /// A delivered mutation may still run after broker cancellation or expiry.
    #[serde(skip)]
    pub outstanding_mutation: bool,
    /// Preserve an admission whose broker reply was lost, including its exact
    /// operation ID and payload, until it can be recovered inside the replay window.
    #[serde(skip)]
    pub outstanding_admission: Option<(amaterasu_module_sdk::Command, Instant)>,
    /// Set only by the completed provider cleanup workflow, never a status label.
    #[serde(skip)]
    pub native_retirement_confirmed: bool,
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
        let mut response = json!({"ok":true,"launch":self});
        response["launch"]["stop_on_stream_end"] = json!(self.stop_on_stream_end());
        response
    }
    fn stop_on_stream_end(&self) -> bool {
        // A transport disconnect suspends streaming, including for Steam games.
        // Explicit stop and application exit still retire the owned process.
        self.terminal_stop_on_stream_end.unwrap_or(false)
    }
    pub fn retire_target(&mut self) {
        self.terminal_stop_on_stream_end = Some(self.stop_on_stream_end());
        self.target = None;
    }
    pub fn preflight_response(&self) -> Value {
        json!({"ok":true,"preflight_token":self.preflight_token,"stop_on_stream_end":self.stop_on_stream_end(),"app_id":self.app_id,"catalog_revision":self.catalog_revision,"config_overrides":self.config_overrides.clone().unwrap_or_default(),"application_display_overrides":self.application_display_overrides,"application_display_settings":self.application_display_settings,"executable_path":self.executable_path,"cleanup_budget_ms":self.cleanup_budget_ms})
    }
}
pub struct Records {
    pub values: BTreeMap<u64, Record>,
    nonce: String,
    operation_epoch: String,
    next_id: u64,
    admitted_operations: BTreeSet<(String, String)>,
    retired_operations: BTreeSet<(String, String)>,
    cleaned_operations: BTreeSet<(String, String)>,
}
impl Records {
    pub fn new() -> Result<Self, &'static str> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| "entropy_unavailable")?;
        let mut operation_epoch = [0; 16];
        getrandom::fill(&mut operation_epoch).map_err(|_| "entropy_unavailable")?;
        Ok(Self {
            values: BTreeMap::new(),
            nonce: hex(&nonce),
            operation_epoch: hex(&operation_epoch),
            next_id: 0,
            admitted_operations: BTreeSet::new(),
            retired_operations: BTreeSet::new(),
            cleaned_operations: BTreeSet::new(),
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
        let tracked_key = if operation_id.starts_with("ale1:") {
            if !self.current_epoch_operation(&operation_id) {
                return Err("stale_operation_epoch");
            }
            let key = (caller.to_owned(), operation_id.clone());
            if self.retired_operations.contains(&key) || self.cleaned_operations.contains(&key) {
                return Err("operation_retired");
            }
            if self.admitted_operations.contains(&key)
                && !self
                    .values
                    .values()
                    .any(|record| record.owner == caller && record.operation_id == operation_id)
            {
                return Err("operation_unconfirmed");
            }
            if !self.admitted_operations.contains(&key)
                && self.admitted_operations.len() + self.retired_operations.len() >= 4096
            {
                return Err("operation_limit");
            }
            Some(key)
        } else {
            None
        };
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
                CatalogSelection::Library => {
                    record.catalog_selection == CatalogSelection::Library
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
        // Reserve the replay fence only when the corresponding record can be
        // installed. Rejected admissions have made no effects and may retry.
        if let Some(tracked_key) = tracked_key {
            self.admitted_operations.insert(tracked_key);
        }
        self.values.insert(
            id,
            Record {
                launch_id: id,
                operation_id,
                app_id,
                library_selection: None,
                preparation_context: None,
                prepared_context_service: None,
                prepared_environment: None,
                state: State::Starting,
                outstanding_request: None,
                outstanding_mutation: false,
                outstanding_admission: None,
                native_retirement_confirmed: false,
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
                terminal_stop_on_stream_end: None,
                output: None,
                global_preparation,
                legacy_global_preparation,
                provider_ticket: None,
                generic_provider_service: None,
                generic_provider_incarnation: None,
                config_overrides: None,
                application_display_settings: None,
                executable_path: None,
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
    pub fn operation_epoch(&self) -> &str {
        &self.operation_epoch
    }

    pub fn current_epoch_operation(&self, operation_id: &str) -> bool {
        tracked_operation_epoch(operation_id) == Some(self.operation_epoch.as_str())
    }

    /// Preserve same-runtime terminal proof after bounded launch records are
    /// pruned. The tracked admission already reserved this tombstone capacity.
    pub fn record_native_retirement(&mut self, id: u64) {
        let Some(record) = self.values.get(&id) else {
            return;
        };
        if !record.native_retirement_confirmed
            || !record.state.terminal()
            || record.cleanup_error.is_some()
            || record.outstanding_request.is_some()
            || record.outstanding_admission.is_some()
            || !self.current_epoch_operation(&record.operation_id)
        {
            return;
        }
        let key = (record.owner.clone(), record.operation_id.clone());
        if self.admitted_operations.contains(&key) {
            self.cleaned_operations.insert(key);
        }
    }

    pub fn native_retired(&self, owner: &str, operation_id: &str) -> bool {
        self.cleaned_operations
            .contains(&(owner.to_owned(), operation_id.to_owned()))
    }

    pub fn tombstone_absent(
        &mut self,
        owner: &str,
        operation_id: &str,
    ) -> Result<(), &'static str> {
        if !self.current_epoch_operation(operation_id) {
            return Err("retirement_unconfirmed");
        }
        let key = (owner.to_owned(), operation_id.to_owned());
        if self.admitted_operations.contains(&key) {
            return Err("retirement_unconfirmed");
        }
        if !self.retired_operations.contains(&key)
            && self.admitted_operations.len() + self.retired_operations.len() >= 4096
        {
            return Err("retirement_unconfirmed");
        }
        self.retired_operations.insert(key);
        Ok(())
    }

    /// Fence a completed, read-only preflight before it can admit a launch.
    pub fn tombstone_unstarted(
        &mut self,
        owner: &str,
        operation_id: &str,
    ) -> Result<(), &'static str> {
        if !self.current_epoch_operation(operation_id) {
            return Err("retirement_unconfirmed");
        }
        let key = (owner.to_owned(), operation_id.to_owned());
        if self.retired_operations.contains(&key) {
            return Ok(());
        }
        let record = self
            .values
            .values_mut()
            .find(|record| record.owner == owner && record.operation_id == operation_id)
            .ok_or("retirement_unconfirmed")?;
        if record.state != State::Starting
            || record.launch_started
            || !record.preflight_ready
            || record.preflight_token.is_none()
            || record.cleanup_error.is_some()
            || record.outstanding_request.is_some()
            || record.outstanding_admission.is_some()
            || record.session_requested
            || record.process_requested
            || record.preparation_requested
            || record.provider_launch_requested
            || record.session_id.is_some()
            || record.process_id.is_some()
            || record.provider_ticket.is_some()
        {
            return Err("retirement_unconfirmed");
        }
        if !self.admitted_operations.remove(&key) {
            return Err("retirement_unconfirmed");
        }
        self.retired_operations.insert(key);
        record.state = State::Cancelled;
        record.preflight_ready = false;
        record.display_admission = None;
        record.retire_target();
        Ok(())
    }

    pub fn is_tombstoned(&self, owner: &str, operation_id: &str) -> bool {
        self.retired_operations
            .contains(&(owner.to_owned(), operation_id.to_owned()))
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

pub(crate) fn tracked_operation_epoch(operation_id: &str) -> Option<&str> {
    let (epoch, nonce) = operation_id.strip_prefix("ale1:")?.split_once(':')?;
    (epoch.len() == 32
        && epoch
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && !nonce.is_empty()
        && nonce.len() <= 80
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)))
    .then_some(epoch)
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

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
