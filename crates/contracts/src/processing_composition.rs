//! Trusted, per-principal bindings for the frame-processing SDK.
//!
//! A service alias chooses an implementation; `contract` and the interface
//! version identify what the implementation must provide. A principal is used
//! only for broker authorization and never establishes interface identity.
use crate::{ApiMethod, valid_name};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const PROCESSING_COMPOSITION_VERSION: u32 = 1;
pub const STREAM_OWNER_CONTRACT: &str = "amaterasu.stream-session/1";
pub const PROCESSING_INTENT_CONTRACT: &str = "amaterasu.processing-intent/1";
pub const SOURCE_PREPARATION_CONTRACT: &str = "amaterasu.processing-source-preparation/1";
pub const MAX_PROCESSING_REGISTRATIONS: usize = 32;
pub const MAX_REGISTRATION_METHODS: usize = 32;
pub const MAX_REGISTRATION_CONSUMERS: usize = 32;
pub const MAX_SETTINGS_GRANTS: usize = 512;

/// Scope grants are installation policy, never request-supplied authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SettingsScopeGrant {
    Host,
    Application(String),
    PairedClient(String),
    Session(String),
    AllApplications,
    AllPairedClients,
    AllSessions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsAccessGrant {
    pub caller: String,
    pub scope: SettingsScopeGrant,
    pub read: bool,
    pub write: bool,
}

/// Installed access to a participant's lifecycle context and settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessingLifecycleGrant {
    pub caller: String,
    pub read_context: bool,
    pub read_frozen_settings: bool,
    pub read_live_settings: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityBinding {
    pub principal: String,
    pub service: String,
    pub contract: String,
    pub interface_major: u32,
    pub interface_minor: u32,
}

impl CapabilityBinding {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_principal(&self.principal)
            || !valid_name(&self.service)
            || !valid_name(&self.contract)
            || self.interface_major == 0
        {
            return Err("invalid_processing_binding");
        }
        Ok(())
    }
}

/// Complete selection held by trusted startup composition. The host derives
/// individual `ProcessingComposition` views from this graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessingSelection {
    pub processor: CapabilityBinding,
    pub frame_consumer: CapabilityBinding,
    /// Explicit per-stream processor configuration when no lifecycle is selected.
    pub frame_settings: Option<Value>,
    pub stream_owner: Option<CapabilityBinding>,
    pub lifecycle: Option<CapabilityBinding>,
    pub settings_registry: Option<CapabilityBinding>,
    pub display_settings: Option<CapabilityBinding>,
    pub foreground: Option<CapabilityBinding>,
    /// Principals that preflight source capture through the selected video contract.
    pub source_preflight_consumers: BTreeSet<String>,
    pub request_ancestors: BTreeSet<String>,
    pub settings_editors: BTreeSet<String>,
    pub prepared_context_consumers: BTreeSet<String>,
    pub lifecycle_admitters: BTreeSet<String>,
    pub history_owners: BTreeSet<String>,
}

impl ProcessingSelection {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.processor.validate()?;
        self.frame_consumer.validate()?;
        for binding in [
            &self.stream_owner,
            &self.lifecycle,
            &self.settings_registry,
            &self.display_settings,
            &self.foreground,
        ]
        .into_iter()
        .flatten()
        {
            binding.validate()?;
        }
        if self.source_preflight_consumers.len() > MAX_REGISTRATION_CONSUMERS
            || self
                .source_preflight_consumers
                .contains(&self.frame_consumer.principal)
            || self.request_ancestors.len() > MAX_REGISTRATION_CONSUMERS
            || self.settings_editors.len() > MAX_REGISTRATION_CONSUMERS
            || self.prepared_context_consumers.len() > MAX_REGISTRATION_CONSUMERS
            || (self.lifecycle.is_none() && !self.prepared_context_consumers.is_empty())
            || self.lifecycle_admitters.len() > MAX_REGISTRATION_CONSUMERS
            || self.history_owners.len() > MAX_REGISTRATION_CONSUMERS
            || self
                .source_preflight_consumers
                .iter()
                .chain(&self.request_ancestors)
                .chain(&self.settings_editors)
                .chain(&self.prepared_context_consumers)
                .chain(&self.lifecycle_admitters)
                .chain(&self.history_owners)
                .any(|name| !valid_principal(name))
            || self.frame_settings.as_ref().is_some_and(|settings| {
                !settings.is_object()
                    || !serde_json::to_vec(settings).is_ok_and(|bytes| bytes.len() <= 8192)
            })
            || (self.lifecycle.is_none() && self.frame_settings.is_none())
            || (self.lifecycle.is_some() && self.frame_settings.is_some())
            || (self.display_settings.is_some() && self.stream_owner.is_none())
            || (self.stream_owner.is_none()
                && (!self.lifecycle_admitters.is_empty() || !self.history_owners.is_empty()))
        {
            return Err("invalid_processing_selection");
        }
        Ok(())
    }
}

/// A plugin-owned descriptor and method declarations supplied to selected
/// registry and lifecycle implementations through the same SDK contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessingRegistration {
    pub provider: String,
    pub descriptor: Value,
    pub methods: Vec<ApiMethod>,
    pub consumers: BTreeSet<String>,
    pub depends_on: Vec<String>,
    pub lifecycle_grants: Vec<ProcessingLifecycleGrant>,
    pub settings_revision: String,
    pub initial_settings: Value,
    pub settings_grants: Vec<SettingsAccessGrant>,
    pub migrations: Vec<Value>,
}

impl ProcessingRegistration {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_principal(&self.provider)
            || !self.descriptor.is_object()
            || self.methods.is_empty()
            || self.methods.len() > MAX_REGISTRATION_METHODS
            || self.consumers.len() > MAX_REGISTRATION_CONSUMERS
            || self.consumers.iter().any(|name| !valid_principal(name))
            || self.depends_on.len() > MAX_PROCESSING_REGISTRATIONS
            || self.depends_on.iter().any(|name| !valid_principal(name))
            || self.lifecycle_grants.len() > MAX_REGISTRATION_CONSUMERS
            || self.lifecycle_grants.iter().any(|grant| {
                !valid_principal(&grant.caller)
                    || (!grant.read_context
                        && !grant.read_frozen_settings
                        && !grant.read_live_settings)
            })
            || self.settings_revision.is_empty()
            || self.settings_revision.len() > 128
            || self.settings_revision.chars().any(char::is_control)
            || !self.initial_settings.is_object()
            || self.settings_grants.len() > MAX_SETTINGS_GRANTS
            || self.migrations.len() > 32
            || self.settings_grants.iter().any(|grant| {
                !valid_principal(&grant.caller)
                    || (!grant.read && !grant.write)
                    || match &grant.scope {
                        SettingsScopeGrant::Application(id)
                        | SettingsScopeGrant::PairedClient(id)
                        | SettingsScopeGrant::Session(id) => {
                            id.is_empty() || id.len() > 256 || id.chars().any(char::is_control)
                        }
                        _ => false,
                    }
            })
        {
            return Err("invalid_processing_registration");
        }
        if self
            .depends_on
            .iter()
            .any(|dependency| dependency == &self.provider)
            || self.depends_on.iter().collect::<BTreeSet<_>>().len() != self.depends_on.len()
            || self
                .lifecycle_grants
                .iter()
                .map(|grant| &grant.caller)
                .collect::<BTreeSet<_>>()
                .len()
                != self.lifecycle_grants.len()
        {
            return Err("invalid_processing_registration_dependencies");
        }
        let mut methods = BTreeSet::new();
        for method in &self.methods {
            if !valid_name(&method.service)
                || !valid_name(&method.method)
                || method.interface_major == 0
                || !methods.insert((&method.service, &method.method, method.interface_major))
            {
                return Err("invalid_processing_registration_method");
            }
        }
        Ok(())
    }
}

/// The trusted host provides an individual, bounded view to each principal.
/// Optional roles are absent unless the composition explicitly selects them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessingComposition {
    pub version: u32,
    pub processor: Option<CapabilityBinding>,
    pub processor_control: Option<CapabilityBinding>,
    pub source_preparation: Option<CapabilityBinding>,
    pub frame_consumer: Option<CapabilityBinding>,
    pub frame_settings: Option<Value>,
    pub stream_owner: Option<CapabilityBinding>,
    pub lifecycle: Option<CapabilityBinding>,
    pub settings_registry: Option<CapabilityBinding>,
    pub display_settings: Option<CapabilityBinding>,
    pub foreground: Option<CapabilityBinding>,
    pub source_preflight_consumers: BTreeSet<String>,
    /// Parent request principals whose authority reaches this stream owner.
    pub request_ancestors: BTreeSet<String>,
    pub prepared_context_consumers: BTreeSet<String>,
    pub lifecycle_admitters: BTreeSet<String>,
    pub history_owners: BTreeSet<String>,
    pub registrations: Vec<ProcessingRegistration>,
}

impl Default for ProcessingComposition {
    fn default() -> Self {
        Self {
            version: PROCESSING_COMPOSITION_VERSION,
            processor: None,
            processor_control: None,
            source_preparation: None,
            frame_consumer: None,
            frame_settings: None,
            stream_owner: None,
            lifecycle: None,
            settings_registry: None,
            display_settings: None,
            foreground: None,
            source_preflight_consumers: BTreeSet::new(),
            request_ancestors: BTreeSet::new(),
            prepared_context_consumers: BTreeSet::new(),
            lifecycle_admitters: BTreeSet::new(),
            history_owners: BTreeSet::new(),
            registrations: Vec::new(),
        }
    }
}

impl ProcessingComposition {
    pub fn bindings(&self) -> impl Iterator<Item = &CapabilityBinding> {
        [
            &self.processor,
            &self.processor_control,
            &self.source_preparation,
            &self.frame_consumer,
            &self.stream_owner,
            &self.lifecycle,
            &self.settings_registry,
            &self.display_settings,
            &self.foreground,
        ]
        .into_iter()
        .flatten()
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != PROCESSING_COMPOSITION_VERSION
            || self.frame_settings.as_ref().is_some_and(|settings| {
                !settings.is_object()
                    || !serde_json::to_vec(settings).is_ok_and(|bytes| bytes.len() <= 8192)
            })
            || self.registrations.len() > MAX_PROCESSING_REGISTRATIONS
            || self.source_preflight_consumers.len() > MAX_REGISTRATION_CONSUMERS
            || self.request_ancestors.len() > MAX_REGISTRATION_CONSUMERS
            || self.prepared_context_consumers.len() > MAX_REGISTRATION_CONSUMERS
            || (self.lifecycle.is_none() && !self.prepared_context_consumers.is_empty())
            || self.lifecycle_admitters.len() > MAX_REGISTRATION_CONSUMERS
            || self.history_owners.len() > MAX_REGISTRATION_CONSUMERS
            || self
                .source_preflight_consumers
                .iter()
                .chain(&self.request_ancestors)
                .chain(&self.prepared_context_consumers)
                .chain(&self.lifecycle_admitters)
                .chain(&self.history_owners)
                .any(|name| !valid_principal(name))
        {
            return Err("invalid_processing_composition");
        }
        for binding in self.bindings() {
            binding.validate()?;
        }
        let mut providers = BTreeSet::new();
        for registration in &self.registrations {
            registration.validate()?;
            if !providers.insert(&registration.provider) {
                return Err("duplicate_processing_registration");
            }
        }
        Ok(())
    }
}

fn valid_principal(principal: &str) -> bool {
    valid_name(principal) && !principal.contains('/')
}
