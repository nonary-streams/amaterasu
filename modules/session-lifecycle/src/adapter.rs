//! Broker adapter for the lifecycle authority. Request payloads do not establish identity.
use crate::prepare_authority::{ControlPolicy, DomainScope, FinalizePolicy};
use amaterasu_extension_api::{ConsumerRequirements, MethodRef, ResourceAccess, SchemaRef};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SERVICE: &str = "amaterasu.session-lifecycle/1";

/// Receives identities assigned by the broker, never identity fields from a request.
pub trait LifecycleHandler: Sized {
    /// Opens and recovers durable authority state before service registration.
    fn from_settings(settings: &LifecycleSettings) -> ModuleResult<Self>;

    fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value;

    /// Advance bounded background work without waiting for a provider. Implementations
    /// must persist an effect's identity and intent before submitting it, and fence
    /// outcomes that cannot be confirmed. Detached broker calls run as this service,
    /// not as the initiator of an earlier request.
    fn advance(
        &mut self,
        _send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        Ok(())
    }
}

/// Installed policy; request payloads cannot supply or amend this selection.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleSettings {
    pub capture_binding_mode: amaterasu_extension_api::CaptureBindingMode,
    pub trusted_callers: BTreeSet<String>,
    /// Installed transport callers allowed to attest an already verified and
    /// retained source reservation for an external capture pipeline.
    pub borrowed_capture_callers: BTreeSet<String>,
    /// Broker identities granted delegation by installed operator policy.
    pub session_delegates: BTreeSet<String>,
    pub input_actions: BTreeSet<String>,
    /// Selected broker principal for new input bindings, when several are authorized.
    pub input_provider: Option<String>,
    pub action_service: String,
    /// Broker identity allowed to attest settings-scope request sources.
    pub settings_registry_caller: String,
    /// Optional registry service used to freeze provider settings at session start.
    pub settings_registry_service: Option<String>,
    pub journal_path: String,
    /// Selects the trusted worker's private state root instead of an installed
    /// absolute path. Requires the host-state-context call grant.
    pub journal_in_runtime_state: bool,
    pub operation_grants: BTreeMap<String, BTreeSet<String>>,
    /// Installed per-caller grants for reading only the named provider snapshots.
    pub provider_settings_readers: BTreeMap<String, BTreeSet<String>>,
    /// Optional installed selection for generic processing source policy.
    pub processing_policy: Option<ProcessingPolicy>,
    /// Processing participants whose settings were supplied directly by composition.
    #[serde(skip)]
    pub static_processing_settings: BTreeSet<String>,
    pub participants: Vec<ParticipantSettings>,
    /// Installed owner-mediated control grants; absent means no control is allowed.
    pub control_policies: Vec<ControlPolicy>,
    /// Exact owner finalization grants, pinned independently from control grants.
    pub finalize_policies: Vec<FinalizePolicy>,
    /// Exact UTF-8 schema documents supplied by installed signed policy.
    pub schema_documents: Vec<SchemaDocument>,
    pub consumers: ConsumerSettings,
    pub consumer_requirements: ConsumerRequirements,
}

impl Default for LifecycleSettings {
    fn default() -> Self {
        Self {
            capture_binding_mode: Default::default(),
            trusted_callers: BTreeSet::new(),
            borrowed_capture_callers: BTreeSet::new(),
            session_delegates: BTreeSet::new(),
            input_actions: BTreeSet::new(),
            input_provider: None,
            action_service: "amaterasu.extension-actions".into(),
            settings_registry_caller: String::new(),
            settings_registry_service: None,
            journal_path: String::new(),
            journal_in_runtime_state: false,
            operation_grants: BTreeMap::new(),
            provider_settings_readers: BTreeMap::new(),
            processing_policy: None,
            static_processing_settings: BTreeSet::new(),
            participants: Vec::new(),
            control_policies: Vec::new(),
            finalize_policies: Vec::new(),
            schema_documents: Vec::new(),
            consumers: ConsumerSettings::default(),
            consumer_requirements: ConsumerRequirements {
                execution_domain: String::new(),
                capture_providers: Vec::new(),
                selector_schemas: Vec::new(),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SchemaDocument {
    pub reference: SchemaRef,
    pub document: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessingPolicy {
    pub provider: String,
    pub service: String,
    pub processor_service: String,
    /// Version 1.1 supplies the requested output range before source selection.
    pub interface_minor: u32,
    /// Participants explicitly advertising the generic source mode contract.
    pub source_mode_providers: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ParticipantSettings {
    pub provider: String,
    /// Wire contract version selected by the installed participant.
    #[serde(default = "default_contract_version")]
    pub contract_version: u32,
    /// Required participants that must prepare before this provider.
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub plan_method: MethodSettings,
    pub callback_method: MethodSettings,
    pub status_method: MethodSettings,
    #[serde(default)]
    pub binding_validate_method: Option<MethodSettings>,
    pub capabilities: BTreeSet<String>,
    pub cleanup_methods: Vec<MethodRef>,
    pub settings_revision: String,
    pub settings: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MethodSettings {
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub schema_revision: u64,
}

impl From<MethodRef> for MethodSettings {
    fn from(method: MethodRef) -> Self {
        Self {
            service: method.service,
            method: method.method,
            interface_major: method.interface_major,
            interface_minor: method.interface_minor,
            schema_revision: method.schema_revision,
        }
    }
}

impl From<amaterasu_module_sdk::processing_composition::LifecycleParticipant>
    for ParticipantSettings
{
    fn from(
        declaration: amaterasu_module_sdk::processing_composition::LifecycleParticipant,
    ) -> Self {
        Self {
            provider: declaration.provider,
            contract_version: declaration.contract_version,
            depends_on: declaration.depends_on,
            plan_method: declaration.plan_method.into(),
            callback_method: declaration.callback_method.into(),
            status_method: declaration.status_method.into(),
            binding_validate_method: declaration.binding_validate_method.map(Into::into),
            capabilities: declaration.capabilities,
            cleanup_methods: declaration.cleanup_methods,
            settings_revision: declaration.settings_revision,
            settings: declaration.settings,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ConsumerSettings {
    pub capture: BTreeSet<String>,
    pub input: BTreeSet<String>,
}

fn default_contract_version() -> u32 {
    1
}

fn valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-' | b'/')
        })
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn valid_schema_reference(schema: &SchemaRef) -> bool {
    valid_opaque_id(&schema.id)
        && schema.major != 0
        && schema.digest.len() == 64
        && schema
            .digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_control_method(method: &MethodRef, service: &str) -> bool {
    method.service == service
        && valid_identifier(&method.method)
        && method.interface_major == 2
        && method.schema_revision != 0
}

fn valid_domain_scope(scope: DomainScope, domains: &BTreeSet<String>) -> bool {
    domains.len() <= 32
        && domains.iter().all(|domain| valid_opaque_id(domain))
        && match scope {
            DomainScope::Static => !domains.is_empty(),
            DomainScope::Resource => domains.is_empty(),
        }
}

impl LifecycleSettings {
    /// Apply provider-neutral processing declarations to this provider's
    /// internal policy. The host never edits these provider-specific fields.
    fn add_processing_composition(
        &mut self,
        composition: &amaterasu_module_sdk::processing_composition::ProcessingComposition,
    ) -> ModuleResult<()> {
        use amaterasu_module_sdk::{media::native_contracts, processing_composition as sdk};
        let selected = composition
            .processor
            .as_ref()
            .ok_or("processing composition has no processor")?;
        sdk::require_contract(selected, native_contracts::FRAME_PROCESSOR, 1, 0)?;
        if let Some(registry) = &composition.settings_registry {
            sdk::require_contract(
                registry,
                amaterasu_extension_api::EXTENSION_REGISTRY_SERVICE,
                1,
                0,
            )?;
            if !self.participants.is_empty()
                && (self
                    .settings_registry_service
                    .as_deref()
                    .is_some_and(|service| service != registry.service)
                    || (!self.settings_registry_caller.is_empty()
                        && self.settings_registry_caller != registry.principal))
            {
                return Err(
                    "processing registry conflicts with other lifecycle participants".into(),
                );
            }
            self.settings_registry_service = Some(registry.service.clone());
            self.settings_registry_caller = registry.principal.clone();
            self.trusted_callers.insert(registry.principal.clone());
            self.operation_grants
                .entry(registry.principal.clone())
                .or_default()
                .insert("resolve_settings_scope".into());
        } else {
            self.static_processing_settings
                .insert(selected.principal.clone());
        }
        for caller in &composition.prepared_context_consumers {
            self.trusted_callers.insert(caller.clone());
            self.operation_grants
                .entry(caller.clone())
                .or_default()
                .insert("read_prepared_plans".into());
        }
        for registration in &composition.registrations {
            let participant = ParticipantSettings::from(sdk::lifecycle_participant(registration)?);
            if self
                .participants
                .iter()
                .any(|old| old.provider == participant.provider)
            {
                return Err("processing participant already configured".into());
            }
            self.participants.push(participant);
            for grant in &registration.lifecycle_grants {
                self.trusted_callers.insert(grant.caller.clone());
                let operations = self
                    .operation_grants
                    .entry(grant.caller.clone())
                    .or_default();
                if grant.read_context {
                    operations.insert("read_provider_context".into());
                }
                if grant.read_frozen_settings {
                    operations.insert("read_frozen_provider_settings".into());
                }
                if grant.read_live_settings {
                    operations.insert("read_live_provider_settings".into());
                }
                if grant.read_frozen_settings || grant.read_live_settings {
                    self.provider_settings_readers
                        .entry(grant.caller.clone())
                        .or_default()
                        .insert(registration.provider.clone());
                }
            }
        }
        let registration = composition
            .registrations
            .iter()
            .find(|registration| registration.provider == selected.principal)
            .ok_or("selected processor has no lifecycle registration")?;
        let intent = sdk::registration_method(registration, "processing_intent")?;
        let control = composition
            .processor_control
            .as_ref()
            .ok_or("selected processor has no processing intent binding")?;
        sdk::require_contract(
            control,
            sdk::PROCESSING_INTENT_CONTRACT,
            intent.interface_major,
            intent.interface_minor,
        )?;
        if control.principal != selected.principal || control.service != intent.service {
            return Err("processing intent differs from selected processor".into());
        }
        let policy = ProcessingPolicy {
            provider: selected.principal.clone(),
            service: intent.service,
            processor_service: selected.service.clone(),
            interface_minor: intent.interface_minor,
            source_mode_providers: BTreeSet::new(),
        };
        if self
            .processing_policy
            .as_ref()
            .is_some_and(|existing| *existing != policy)
        {
            return Err("processing policy conflicts with lifecycle settings".into());
        }
        self.processing_policy = Some(policy);
        Ok(())
    }

    /// Compile only policy-supplied local documents; no runtime fetch or request
    /// payload can add a schema. Every control and finalization pin must resolve exactly.
    pub(crate) fn schema_catalog(
        &self,
    ) -> ModuleResult<amaterasu_extension_api::schemas::SchemaCatalog> {
        if self.schema_documents.len() > 32
            || self
                .schema_documents
                .iter()
                .any(|entry| entry.document.len() > 12_000)
            || self
                .schema_documents
                .iter()
                .map(|entry| entry.document.len())
                .sum::<usize>()
                > 12_000
        {
            return Err("lifecycle schema document limit exceeded".into());
        }
        let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
            self.schema_documents
                .iter()
                .map(|entry| (entry.reference.clone(), entry.document.as_str())),
        )?;
        if self.control_policies.iter().any(|policy| {
            !catalog.contains(&policy.argument_schema) || !catalog.contains(&policy.output_schema)
        }) || self
            .finalize_policies
            .iter()
            .any(|policy| !catalog.contains(&policy.output_schema))
        {
            return Err("lifecycle policy schema unavailable".into());
        }
        Ok(catalog)
    }

    /// Validate installed policy shape and graph binding. Resolving the actual
    /// schema bytes and installed method approvals remains a separate authority check.
    fn validate_control_policies(&self) -> ModuleResult<()> {
        if self.control_policies.len() > 128 {
            return Err("too many lifecycle control policies".into());
        }
        for (index, policy) in self.control_policies.iter().enumerate() {
            let dependent = self
                .participants
                .iter()
                .find(|p| p.provider == policy.dependent_provider);
            let owner = self
                .participants
                .iter()
                .find(|p| p.provider == policy.owner_provider);
            if dependent.is_none_or(|p| !p.depends_on.contains(&policy.owner_provider))
                || owner.is_none()
                || self.control_policies[..index].contains(policy)
                || !valid_identifier(&policy.owner_service)
                || !valid_control_method(&policy.owner_method, &policy.owner_service)
                || !valid_control_method(&policy.status_method, &policy.owner_service)
                || !valid_control_method(&policy.settlement_method, &policy.owner_service)
                || !valid_opaque_id(&policy.output_capability)
                || !valid_schema_reference(&policy.argument_schema)
                || !valid_schema_reference(&policy.output_schema)
                || matches!(policy.max_access, ResourceAccess::Read)
                || !valid_domain_scope(policy.domain_scope, &policy.mutation_domains)
            {
                return Err("invalid lifecycle control policy".into());
            }
        }
        Ok(())
    }

    fn validate_finalize_policies(&self) -> ModuleResult<()> {
        if self.finalize_policies.len() > 128 {
            return Err("too many lifecycle finalize policies".into());
        }
        for (index, policy) in self.finalize_policies.iter().enumerate() {
            if !self
                .participants
                .iter()
                .any(|p| p.provider == policy.owner_provider)
                || self.finalize_policies[..index].contains(policy)
                || !valid_identifier(&policy.owner_service)
                || !valid_control_method(&policy.method, &policy.owner_service)
                || !valid_control_method(&policy.status_method, &policy.owner_service)
                || !valid_opaque_id(&policy.output_capability)
                || !valid_schema_reference(&policy.output_schema)
                || !self
                    .consumer_requirements
                    .capture_providers
                    .contains(&policy.capture_provider)
                || !self
                    .consumer_requirements
                    .selector_schemas
                    .contains(&policy.selector_schema)
                || matches!(policy.required_access, ResourceAccess::Read)
                || !valid_domain_scope(policy.domain_scope, &policy.mutation_domains)
            {
                return Err("invalid lifecycle finalize policy".into());
            }
        }
        Ok(())
    }

    /// Bind versioned policy endpoints to the selected participant contracts.
    /// Graph, schema, and installed approval checks remain independent checks.
    fn validate_contract_profile(&self) -> ModuleResult<()> {
        let uses_v2 = self.participants.iter().any(|p| p.contract_version == 2);
        if !uses_v2 {
            return Ok(());
        }
        let selected_owner = |provider: &str, service: &str| {
            self.participants.iter().any(|participant| {
                participant.provider == provider
                    && participant.contract_version == 2
                    && participant.plan_method.service == service
                    && participant.callback_method.service == service
                    && participant.status_method.service == service
            })
        };
        if self
            .participants
            .iter()
            .any(|participant| participant.contract_version != 2)
            || self.finalize_policies.is_empty()
            || self
                .control_policies
                .iter()
                .any(|policy| !selected_owner(&policy.owner_provider, &policy.owner_service))
            || self
                .finalize_policies
                .iter()
                .any(|policy| !selected_owner(&policy.owner_provider, &policy.owner_service))
        {
            return Err("unsupported v2 lifecycle profile".into());
        }
        Ok(())
    }

    /// Resolve required dependencies before admission. Among ready participants,
    /// the earliest configured provider wins, preserving deterministic tie order.
    pub(crate) fn resolved_participants(&self) -> ModuleResult<Vec<String>> {
        if self.participants.len() > 32 {
            return Err("too many lifecycle participants".into());
        }
        let mut names = BTreeSet::new();
        for participant in &self.participants {
            if !valid_identifier(&participant.provider)
                || !names.insert(participant.provider.as_str())
            {
                return Err("invalid lifecycle participant".into());
            }
        }
        for participant in &self.participants {
            let mut seen = BTreeSet::new();
            if participant.depends_on.len() > 32
                || participant.depends_on.iter().any(|dependency| {
                    !valid_identifier(dependency)
                        || dependency == &participant.provider
                        || !names.contains(dependency.as_str())
                        || !seen.insert(dependency)
                })
            {
                return Err("invalid lifecycle participant dependency".into());
            }
        }
        let mut resolved = Vec::with_capacity(self.participants.len());
        let mut visited = BTreeSet::new();
        while resolved.len() < self.participants.len() {
            let Some(participant) = self.participants.iter().find(|participant| {
                !visited.contains(participant.provider.as_str())
                    && participant
                        .depends_on
                        .iter()
                        .all(|dependency| visited.contains(dependency.as_str()))
            }) else {
                return Err("cyclic lifecycle participant dependencies".into());
            };
            visited.insert(participant.provider.as_str());
            resolved.push(participant.provider.clone());
        }
        Ok(resolved)
    }

    pub(crate) fn validate(&self) -> ModuleResult<()> {
        if self.processing_policy.as_ref().is_some_and(|policy| {
            policy.interface_minor != 1
                || !valid_identifier(&policy.provider)
                || !valid_identifier(&policy.service)
                || !valid_identifier(&policy.processor_service)
                || !self
                    .participants
                    .iter()
                    .any(|p| p.provider == policy.provider)
                || (policy.source_mode_providers.is_empty()
                    && self.capture_binding_mode.is_managed())
                || policy.source_mode_providers.iter().any(|id| {
                    !self.participants.iter().any(|p| {
                        &p.provider == id && p.capabilities.contains("source_video_mode/1")
                    })
                })
                || self
                    .finalize_policies
                    .iter()
                    .any(|p| !policy.source_mode_providers.contains(&p.owner_provider))
        }) {
            return Err("invalid lifecycle processing policy".into());
        }
        if self.journal_path.is_empty() != self.journal_in_runtime_state {
            return Err("select exactly one lifecycle journal location".into());
        }
        let requirements = &self.consumer_requirements;
        if requirements.execution_domain.is_empty()
            || requirements.execution_domain.len() > 256
            || requirements.execution_domain.chars().any(char::is_control)
            || [
                &requirements.capture_providers,
                &requirements.selector_schemas,
            ]
            .into_iter()
            .any(|values| {
                (values.is_empty() != self.capture_binding_mode.is_external())
                    || values.len() > 32
                    || values.iter().any(|value| {
                        value.is_empty()
                            || value.len() > 128
                            || !value.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/')
                            })
                    })
            })
        {
            return Err("invalid lifecycle consumer requirements".into());
        }
        if self.capture_binding_mode.is_external()
            && (!self.control_policies.is_empty()
                || !self.finalize_policies.is_empty()
                || self
                    .participants
                    .iter()
                    .any(|participant| participant.contract_version != 1))
        {
            return Err("external capture cannot select capture composition".into());
        }
        if self.trusted_callers.len() > 256
            || self
                .trusted_callers
                .iter()
                .any(|caller| !valid_identifier(caller))
        {
            return Err("invalid lifecycle trusted callers".into());
        }
        if self.borrowed_capture_callers.len() > 32
            || (!self.borrowed_capture_callers.is_empty()
                && !self.capture_binding_mode.is_external())
            || self
                .borrowed_capture_callers
                .iter()
                .any(|caller| !valid_identifier(caller) || !self.trusted_callers.contains(caller))
        {
            return Err("invalid lifecycle borrowed capture callers".into());
        }
        if self.session_delegates.len() > 128
            || self.session_delegates.iter().any(|delegate| {
                !valid_identifier(delegate) || !self.trusted_callers.contains(delegate)
            })
        {
            return Err("invalid lifecycle session delegates".into());
        }
        if !valid_identifier(&self.action_service)
            || self.input_actions.len() > 32
            || self
                .input_actions
                .iter()
                .any(|action| !valid_identifier(action))
        {
            return Err("invalid lifecycle input action policy".into());
        }
        if self
            .settings_registry_service
            .as_deref()
            .is_some_and(|service| !valid_identifier(service))
        {
            return Err("invalid lifecycle settings registry service".into());
        }
        if !self.settings_registry_caller.is_empty()
            && (!valid_identifier(&self.settings_registry_caller)
                || !self
                    .trusted_callers
                    .contains(&self.settings_registry_caller))
        {
            return Err("invalid lifecycle settings registry caller".into());
        }
        if self.operation_grants.iter().any(|(caller, operations)| {
            !self.trusted_callers.contains(caller)
                || operations.len() > 128
                || operations
                    .iter()
                    .any(|operation| !valid_identifier(operation))
        }) {
            return Err("invalid lifecycle operation grants".into());
        }
        for consumers in [&self.consumers.capture, &self.consumers.input] {
            if consumers.len() > 128
                || consumers.iter().any(|caller| {
                    !valid_identifier(caller) || !self.trusted_callers.contains(caller)
                })
            {
                return Err("invalid lifecycle consumers".into());
            }
        }
        if self
            .input_provider
            .as_ref()
            .is_some_and(|provider| !self.consumers.input.contains(provider))
        {
            return Err("invalid lifecycle input provider".into());
        }
        let mut providers = BTreeSet::new();
        if self.participants.len() > 32 {
            return Err("too many lifecycle participants".into());
        }
        for participant in &self.participants {
            if !valid_identifier(&participant.provider)
                || !matches!(participant.contract_version, 1 | 2)
                || !providers.insert(&participant.provider)
                || participant.capabilities.len() > 32
                || participant
                    .capabilities
                    .iter()
                    .any(|id| !valid_identifier(id))
                || participant.cleanup_methods.len() > 32
                || participant
                    .cleanup_methods
                    .iter()
                    .enumerate()
                    .any(|(index, method)| {
                        !valid_identifier(&method.service)
                            || !valid_identifier(&method.method)
                            || method.interface_major == 0
                            || method.schema_revision == 0
                            || participant.cleanup_methods[..index].contains(method)
                    })
                || !valid_identifier(&participant.settings_revision)
                || !participant.settings.is_object()
                || serde_json::to_vec(&participant.settings)
                    .map_err(|e| e.to_string())?
                    .len()
                    > 8192
            {
                return Err("invalid lifecycle participant".into());
            }
            for method in [
                &participant.plan_method,
                &participant.callback_method,
                &participant.status_method,
            ]
            .into_iter()
            .chain(participant.binding_validate_method.iter())
            {
                if !valid_identifier(&method.service)
                    || !valid_identifier(&method.method)
                    || method.interface_major != participant.contract_version
                    || method.schema_revision == 0
                {
                    return Err("invalid lifecycle participant method".into());
                }
            }
        }
        if self.provider_settings_readers.len() > 128
            || self
                .provider_settings_readers
                .iter()
                .any(|(caller, allowed)| {
                    !self.trusted_callers.contains(caller)
                        || allowed.len() > 32
                        || allowed.iter().any(|provider| !providers.contains(provider))
                })
        {
            return Err("invalid lifecycle provider settings readers".into());
        }
        self.resolved_participants()?;
        self.validate_contract_profile()?;
        self.validate_control_policies()?;
        self.validate_finalize_policies()?;
        self.schema_catalog()?;
        Ok(())
    }
}

pub struct LifecycleAdapter<H> {
    runtime: RuntimeClient,
    service: String,
    trusted_callers: BTreeSet<String>,
    operation_grants: BTreeMap<String, BTreeSet<String>>,
    handler: H,
}

impl<H: LifecycleHandler> ManagedModule for LifecycleAdapter<H> {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("lifecycle settings unavailable".into());
        };
        let mut settings: LifecycleSettings =
            serde_json::from_value(payload).map_err(|_| "invalid lifecycle settings".to_owned())?;
        let service = if let Some(composition) = runtime.processing_composition()? {
            let binding = composition
                .lifecycle
                .as_ref()
                .ok_or("lifecycle processing composition lacks its selected binding")?;
            let service = amaterasu_module_sdk::processing_composition::require_contract(
                binding,
                amaterasu_extension_api::SESSION_LIFECYCLE_SERVICE,
                1,
                0,
            )?
            .to_owned();
            settings.add_processing_composition(&composition)?;
            service
        } else {
            SERVICE.to_owned()
        };
        settings.validate()?;
        resolve_journal_location(&mut settings, || runtime.runtime_state_root())?;
        let handler = H::from_settings(&settings)?;
        runtime.require_success(Command::Register {
            service: service.clone(),
        })?;
        Ok(Self {
            runtime,
            service,
            trusted_callers: settings.trusted_callers,
            operation_grants: settings.operation_grants,
            handler,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.handler
            .advance(&mut |command| self.runtime.send(command))?;
        let (id, caller, initiator, service, payload) = match self.runtime.send(Command::Poll {})? {
            Response::Pending => return Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
                service,
                payload,
                ..
            } => (id, caller, initiator, service, payload),
            response => return Err(format!("unexpected lifecycle poll response: {response:?}")),
        };
        match self.runtime.send(Command::CheckRequest { request: id })? {
            Response::Ok => {}
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                return Ok(());
            }
            response => {
                return Err(format!(
                    "unexpected lifecycle request check response: {response:?}"
                ));
            }
        }
        let response = if service != self.service {
            json!({"ok": false, "code": "invalid_service"})
        } else if !self.trusted_callers.contains(&caller)
            || !payload
                .get("operation")
                .and_then(Value::as_str)
                .is_some_and(|operation| {
                    self.operation_grants
                        .get(&caller)
                        .is_some_and(|grants| grants.contains(operation))
                })
        {
            json!({"ok": false, "code": "permission_denied"})
        } else {
            self.handler.handle(&caller, &initiator, payload)
        };
        // The broker bounds each result independently. Return an explicit failure
        // instead of letting an oversized result terminate the lifecycle module;
        // any durable transition remains available for reconciliation.
        let response = if serde_json::to_vec(&response)
            .is_ok_and(|bytes| bytes.len() <= amaterasu_module_sdk::MAX_BROKER_RESULT_BYTES)
        {
            response
        } else {
            json!({"status":"rejected", "reason":"response_too_large"})
        };
        match self.runtime.send(Command::Complete {
            request: id,
            payload: response,
        })? {
            Response::Ok => Ok(()),
            // A committed transition survives a request cancelled after dispatch.
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            Response::Error { code, message }
                if code == "invalid_request" && message == "result payload is too large" =>
            {
                // Older runtimes enforce a smaller limit. Preserve the module
                // and return an honest failure without replaying the handler.
                match self.runtime.send(Command::Complete {
                    request: id,
                    payload: json!({"status":"rejected", "reason":"response_too_large"}),
                })? {
                    Response::Ok => Ok(()),
                    Response::Error { code, .. }
                        if code == "request_ended" || code == "not_found" =>
                    {
                        Ok(())
                    }
                    response => Err(format!(
                        "unexpected lifecycle completion response: {response:?}"
                    )),
                }
            }
            response => Err(format!(
                "unexpected lifecycle completion response: {response:?}"
            )),
        }
    }
}

fn resolve_journal_location(
    settings: &mut LifecycleSettings,
    state_root: impl FnOnce() -> ModuleResult<std::path::PathBuf>,
) -> ModuleResult<()> {
    if !settings.journal_in_runtime_state {
        return Ok(());
    }
    let root = state_root()?;
    if !root.is_absolute() {
        return Err("runtime state root must be absolute".into());
    }
    let directory = root.join("session-lifecycle");
    amaterasu_storage::create_private_directory(&directory)?;
    settings.journal_path = directory
        .join("journal.json")
        .to_str()
        .ok_or("lifecycle journal path is not UTF-8")?
        .to_owned();
    settings.journal_in_runtime_state = false;
    Ok(())
}

#[cfg(test)]
mod migrated_policy_tests {
    use super::*;

    fn migrated_policy() -> serde_json::Value {
        let path = std::env::var("AMATERASU_MIGRATED_REPORT").unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        report["optional_bootstrap"]["policy"].clone()
    }

    #[test]
    #[ignore = "needs AMATERASU_MIGRATED_REPORT from the release review"]
    fn reviewed_installed_policy_starts_lifecycle() {
        let policy = migrated_policy();
        let id = "amaterasu.session-lifecycle";
        let mut settings: LifecycleSettings =
            serde_json::from_value(policy["settings"][id].clone()).unwrap();
        let composition =
            serde_json::from_value(policy["processing_compositions"][id].clone()).unwrap();
        settings.add_processing_composition(&composition).unwrap();
        settings.validate().unwrap();
        assert_eq!(
            settings
                .participants
                .iter()
                .filter(|participant| participant.provider == "amaterasu.rtx-hdr")
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
    use amaterasu_module_sdk::processing_composition::{
        CapabilityBinding, ProcessingComposition, ProcessingLifecycleGrant, ProcessingRegistration,
    };
    use std::collections::VecDeque;
    use std::ffi::c_void;

    #[test]
    fn processing_registration_populates_lifecycle_from_the_sdk_contract() {
        let binding = CapabilityBinding {
            principal: "example.effect".into(),
            service: "example.effect.native/1".into(),
            contract: amaterasu_module_sdk::media::native_contracts::FRAME_PROCESSOR.into(),
            interface_major: 1,
            interface_minor: 0,
        };
        let methods = [
            "settings_validate",
            "resource_plan",
            "lifecycle_callback",
            "lifecycle_status",
            "processing_intent",
        ]
        .into_iter()
        .map(|name| {
            let mut method =
                amaterasu_module_sdk::operation_api::method("example.effect/1", name, &[], &[]);
            if name == "processing_intent" {
                method.interface_minor = 1;
            }
            method
        })
        .collect();
        let registration = ProcessingRegistration {
            provider: "example.effect".into(),
            descriptor: json!({"provider":"example.effect","revision":1,"options":[],"actions":[],"capabilities":[],"settings_validate":{"service":"example.effect/1","method":"settings_validate","interface_major":1,"interface_minor":0,"schema_revision":1}}),
            methods,
            consumers: ["example.stream".into()].into(),
            depends_on: vec![],
            lifecycle_grants: vec![ProcessingLifecycleGrant {
                caller: "example.stream".into(),
                read_context: false,
                read_frozen_settings: true,
                read_live_settings: false,
            }],
            settings_revision: "revision".into(),
            initial_settings: json!({}),
            settings_grants: vec![],
            migrations: vec![],
        };
        let composition = ProcessingComposition {
            processor: Some(binding),
            processor_control: Some(CapabilityBinding {
                principal: "example.effect".into(),
                service: "example.effect/1".into(),
                contract: amaterasu_module_sdk::processing_composition::PROCESSING_INTENT_CONTRACT
                    .into(),
                interface_major: 1,
                interface_minor: 1,
            }),
            registrations: vec![registration],
            ..Default::default()
        };
        let mut settings = LifecycleSettings::default();
        settings.add_processing_composition(&composition).unwrap();
        assert_eq!(settings.participants[0].provider, "example.effect");
        assert_eq!(
            settings
                .processing_policy
                .as_ref()
                .unwrap()
                .processor_service,
            "example.effect.native/1"
        );
        assert!(
            settings.operation_grants["example.stream"].contains("read_frozen_provider_settings")
        );
        assert!(settings.provider_settings_readers["example.stream"].contains("example.effect"));
        assert!(
            !settings.operation_grants["example.stream"].contains("read_live_provider_settings")
        );
    }

    #[derive(Default)]
    struct Handler;

    impl LifecycleHandler for Handler {
        fn from_settings(_settings: &LifecycleSettings) -> ModuleResult<Self> {
            Ok(Self)
        }

        fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value {
            json!({"caller": caller, "initiator": initiator, "payload": payload})
        }
    }

    struct Broker {
        settings: Value,
        requests: VecDeque<Response>,
        completions: Vec<Value>,
        active: bool,
        completion_error: Option<&'static str>,
        legacy_result_limit: bool,
        advances: usize,
    }

    impl Broker {
        fn dispatch(&mut self, command: Command) -> Response {
            match command {
                Command::GetSettings {} => Response::Result {
                    payload: self.settings.clone(),
                },
                Command::GetProcessingComposition {} => {
                    Response::error("not_found", "no processing selection")
                }
                Command::Register { .. } => Response::Ok,
                Command::Log { .. } => {
                    self.advances += 1;
                    Response::Ok
                }
                Command::Poll {} => self.requests.pop_front().unwrap_or(Response::Pending),
                Command::CheckRequest { .. } if self.active => Response::Ok,
                Command::CheckRequest { .. } => Response::error("request_ended", "cancelled"),
                Command::Complete { payload, .. } => {
                    if self.legacy_result_limit
                        && serde_json::to_vec(&payload).unwrap().len() > 16_384
                    {
                        self.completions.push(payload);
                        return Response::error("invalid_request", "result payload is too large");
                    }
                    self.completions.push(payload);
                    match self.completion_error {
                        Some(code) => Response::error(code, "completion failed"),
                        None => Response::Ok,
                    }
                }
                _ => panic!("unexpected broker command"),
            }
        }
    }

    unsafe extern "C" fn invoke(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> abi::Status {
        // SAFETY: the test keeps exclusive access to the broker through this endpoint.
        let broker = unsafe { &mut *context.cast::<Broker>() };
        // SAFETY: the SDK supplies bytes valid for the duration of this call.
        let Ok(bytes) = (unsafe { input.read() }) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice(bytes) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Ok(output) = serde_json::to_vec(&broker.dispatch(command)) else {
            return abi::STATUS_INTERNAL_ERROR;
        };
        // SAFETY: the reply sink copies these bytes synchronously.
        unsafe { reply.send(&output) };
        abi::STATUS_OK
    }

    fn request(caller: &str, initiator: &str) -> Response {
        Response::Request {
            id: 1,
            caller: caller.into(),
            initiator: initiator.into(),
            service: SERVICE.into(),
            payload: json!({"operation":"prepare", "caller":"transport", "initiator":"forged"}),
            remaining_ms: 1000,
        }
    }

    fn run(broker: &mut Broker) -> ModuleResult<()> {
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (broker as *mut Broker).cast(),
            invoke,
        };
        // SAFETY: broker outlives the module and is exclusively accessed through the endpoint.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = LifecycleAdapter::<Handler>::start(runtime)?;
        module.tick()
    }

    fn broker(mut settings: Value, caller: &str) -> Broker {
        if let Some(object) = settings.as_object_mut() {
            object
                .entry("journal_path")
                .or_insert(json!("/test/journal"));
            object.entry("consumer_requirements").or_insert(json!({"execution_domain":"desktop", "capture_providers":["capture"], "selector_schemas":["selector/1"]}));
        }
        Broker {
            settings,
            requests: VecDeque::from([request(caller, "real-initiator")]),
            completions: Vec::new(),
            active: true,
            completion_error: None,
            legacy_result_limit: false,
            advances: 0,
        }
    }

    #[test]
    fn broker_identity_is_authority_and_empty_policy_denies() {
        let mut denied = broker(json!({}), "transport");
        run(&mut denied).unwrap();
        assert_eq!(denied.completions[0]["code"], "permission_denied");
        let mut denied = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "plugin",
        );
        run(&mut denied).unwrap();
        assert_eq!(denied.completions[0]["code"], "permission_denied");
        let mut no_grant = broker(json!({"trusted_callers":["transport"]}), "transport");
        run(&mut no_grant).unwrap();
        assert_eq!(no_grant.completions[0]["code"], "permission_denied");
        let mut allowed = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        run(&mut allowed).unwrap();
        assert_eq!(allowed.completions[0]["caller"], "transport");
        assert_eq!(allowed.completions[0]["initiator"], "real-initiator");
    }

    #[test]
    fn oversized_result_is_reported_without_terminating_the_module() {
        let mut broker = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        if let Some(Response::Request { payload, .. }) = broker.requests.front_mut() {
            payload["large"] = json!("x".repeat(amaterasu_module_sdk::MAX_BROKER_RESULT_BYTES));
        }
        run(&mut broker).unwrap();
        assert_eq!(broker.completions[0]["reason"], "response_too_large");
    }

    #[test]
    fn older_broker_limit_returns_failure_without_replaying_handler() {
        let mut broker = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        broker.legacy_result_limit = true;
        if let Some(Response::Request { payload, .. }) = broker.requests.front_mut() {
            payload["large"] = json!("x".repeat(20_000));
        }
        run(&mut broker).unwrap();
        assert_eq!(broker.completions.len(), 2);
        assert_eq!(broker.completions[1]["reason"], "response_too_large");
    }

    #[test]
    fn cancelled_request_is_not_dispatched_and_late_cancellation_is_tolerated() {
        let mut cancelled = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        cancelled.active = false;
        run(&mut cancelled).unwrap();
        assert!(cancelled.completions.is_empty());
        for code in ["request_ended", "not_found", "permission_denied"] {
            let mut late = broker(
                json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
                "transport",
            );
            late.completion_error = Some(code);
            assert_eq!(run(&mut late).is_ok(), code != "permission_denied");
            assert_eq!(late.completions.len(), 1);
        }
    }

    #[test]
    fn runtime_journal_selection_is_explicit_and_has_no_fallback() {
        let mut settings: LifecycleSettings = serde_json::from_value(json!({
            "journal_in_runtime_state":true,
            "capture_binding_mode":"external",
            "consumer_requirements":{"execution_domain":"desktop", "capture_providers":[], "selector_schemas":[]}
        })).unwrap();
        assert!(settings.validate().is_ok());
        let mut ambiguous = settings.clone();
        ambiguous.journal_path = "/operator/journal".into();
        assert!(ambiguous.validate().is_err());
        assert!(
            resolve_journal_location(&mut settings, || Err("permission_denied".into())).is_err()
        );
        assert!(settings.journal_path.is_empty());
        assert!(settings.journal_in_runtime_state);
        assert!(resolve_journal_location(&mut settings, || Ok("relative".into())).is_err());
        let root = std::env::temp_dir().join(format!(
            "amaterasu-runtime-journal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        resolve_journal_location(&mut settings, || Ok(root.clone())).unwrap();
        assert_eq!(
            std::path::Path::new(&settings.journal_path),
            root.join("session-lifecycle/journal.json")
        );
        assert!(!settings.journal_in_runtime_state);
        assert!(settings.validate().is_ok());
        assert!(root.join("session-lifecycle").is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn processing_selection_requires_source_provider_for_managed_capture() {
        let method = json!({"service":"provider/1","method":"plan","interface_major":1,"interface_minor":0,"schema_revision":1});
        let participant = json!({"provider":"provider", "plan_method":method,
            "callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],
            "settings_revision":"initial","settings":{}});
        let mut settings: LifecycleSettings = serde_json::from_value(json!({
            "journal_in_runtime_state":true,
            "capture_binding_mode":"external",
            "consumer_requirements":{"execution_domain":"desktop", "capture_providers":[], "selector_schemas":[]},
            "participants":[participant],
            "processing_policy":{"provider":"provider","service":"provider/1",
                "processor_service":"processor/1", "interface_minor":1, "source_mode_providers":[]}
        })).unwrap();
        assert!(settings.validate().is_ok());
        settings.capture_binding_mode = amaterasu_extension_api::CaptureBindingMode::Managed;
        settings.consumer_requirements.capture_providers = vec!["capture".into()];
        settings.consumer_requirements.selector_schemas = vec!["selector/1".into()];
        assert!(settings.validate().is_err());
        settings
            .processing_policy
            .as_mut()
            .unwrap()
            .source_mode_providers
            .insert("provider".into());
        assert!(settings.validate().is_err());
        settings.participants[0]
            .capabilities
            .insert("source_video_mode/1".into());
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn provider_settings_readers_require_trusted_caller_and_selected_provider() {
        let mut settings: LifecycleSettings = serde_json::from_value(json!({
            "journal_in_runtime_state":true,
            "capture_binding_mode":"external",
            "consumer_requirements":{"execution_domain":"desktop", "capture_providers":[], "selector_schemas":[]}
        })).unwrap();
        settings
            .provider_settings_readers
            .insert("consumer".into(), ["missing".into()].into());
        assert!(settings.validate().is_err());
        settings.trusted_callers.insert("consumer".into());
        assert!(settings.validate().is_err());
        settings.provider_settings_readers.clear();
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn malformed_settings_fail_closed() {
        for settings in [
            json!({"trusted_callers":[""]}),
            json!({"journal_path":""}),
            json!({"trusted_callers":"transport"}),
            json!({"unexpected":true}),
        ] {
            let mut broker = broker(settings, "transport");
            assert!(run(&mut broker).is_err());
            assert!(broker.completions.is_empty());
        }
    }
    #[test]
    fn background_work_advances_on_idle_and_busy_ticks() {
        struct Advancing;
        impl LifecycleHandler for Advancing {
            fn from_settings(_: &LifecycleSettings) -> ModuleResult<Self> {
                Ok(Self)
            }
            fn handle(&mut self, _: &str, _: &str, _: Value) -> Value {
                json!({"ok": true})
            }
            fn advance(
                &mut self,
                send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
            ) -> ModuleResult<()> {
                match send(Command::Log {
                    level: 0,
                    target: "test".into(),
                    message: "advance".into(),
                })? {
                    Response::Ok => Ok(()),
                    _ => Err("advance failed".into()),
                }
            }
        }
        let mut broker = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut broker as *mut Broker).cast(),
            invoke,
        };
        // SAFETY: broker outlives the module and is exclusively accessed through the endpoint.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = LifecycleAdapter::<Advancing>::start(runtime).unwrap();
        module.tick().unwrap();
        module.tick().unwrap();
        drop(module);
        assert_eq!(broker.advances, 2);
        assert_eq!(broker.completions.len(), 1);
    }
    #[test]
    fn participants_and_consumers_require_bounded_trusted_policy() {
        let method = json!({"service":"provider/1", "method":"prepare", "interface_major":1, "interface_minor":0, "schema_revision":1});
        let participant = json!({"provider":"provider", "plan_method":method, "callback_method":method, "status_method":method, "capabilities":["display/change"], "cleanup_methods":[method], "settings_revision":"rev-1", "settings":{}});
        let policy = json!({"journal_path":"/test/journal", "trusted_callers":["capture", "input"], "consumers":{"capture":["capture"], "input":["input"]}, "participants":[participant], "consumer_requirements":{"execution_domain":"desktop", "capture_providers":["capture"], "selector_schemas":["selector/1"]}});
        let parse = |value| {
            serde_json::from_value::<LifecycleSettings>(value)
                .unwrap()
                .validate()
        };
        assert!(parse(policy.clone()).is_ok());
        let mut selected_input = policy.clone();
        selected_input["input_provider"] = json!("input");
        assert!(parse(selected_input.clone()).is_ok());
        selected_input["input_provider"] = json!("foreign");
        assert!(parse(selected_input).is_err());
        let mut invalid_dependency = policy.clone();
        invalid_dependency["participants"][0]["depends_on"] = json!(["absent"]);
        assert!(parse(invalid_dependency).is_err());
        let mut registry_service = policy.clone();
        registry_service["settings_registry_service"] = json!("amaterasu.extension-registry/1");
        assert!(parse(registry_service.clone()).is_ok());
        registry_service["settings_registry_service"] = json!("");
        assert!(parse(registry_service).is_err());
        let mut registry = policy.clone();
        registry["settings_registry_caller"] = json!("input");
        assert!(parse(registry).is_ok());
        let mut untrusted_registry = policy.clone();
        untrusted_registry["settings_registry_caller"] = json!("foreign");
        assert!(parse(untrusted_registry).is_err());
        let mut actions = policy.clone();
        actions["input_actions"] = json!(["display/remove"]);
        actions["action_service"] = json!("amaterasu.extension-actions");
        assert!(parse(actions).is_ok());
        let mut invalid_action = policy.clone();
        invalid_action["input_actions"] = json!(["invalid\naction"]);
        assert!(parse(invalid_action).is_err());
        let mut invalid_service = policy.clone();
        invalid_service["action_service"] = json!("");
        assert!(parse(invalid_service).is_err());
        let mut many_actions = policy.clone();
        many_actions["input_actions"] =
            json!((0..33).map(|n| format!("action-{n}")).collect::<Vec<_>>());
        assert!(parse(many_actions).is_err());
        let mut binding = policy.clone();
        binding["participants"][0]["binding_validate_method"] = method.clone();
        assert!(parse(binding.clone()).is_ok());
        binding["participants"][0]["binding_validate_method"]["interface_major"] = json!(0);
        assert!(parse(binding).is_err());
        let mut binding = policy.clone();
        binding["participants"][0]["binding_validate_method"] = method.clone();
        binding["participants"][0]["binding_validate_method"]["schema_revision"] = json!(0);
        assert!(parse(binding).is_err());
        let mut delegated = policy.clone();
        delegated["session_delegates"] = json!(["input"]);
        assert!(parse(delegated).is_ok());
        let mut untrusted_delegate = policy.clone();
        untrusted_delegate["session_delegates"] = json!(["other"]);
        assert!(parse(untrusted_delegate).is_err());
        let mut too_many_delegates = policy.clone();
        let delegates: Vec<_> = (0..129).map(|index| format!("delegate-{index}")).collect();
        too_many_delegates["trusted_callers"] = json!(delegates);
        too_many_delegates["session_delegates"] = json!(delegates);
        too_many_delegates["consumers"] = json!({});
        assert!(parse(too_many_delegates).is_err());
        for field in ["capabilities", "cleanup_methods"] {
            let mut empty = policy.clone();
            empty["participants"][0][field] = json!([]);
            assert!(parse(empty).is_ok());
            let mut missing = policy.clone();
            missing["participants"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(serde_json::from_value::<LifecycleSettings>(missing).is_err());
        }
        let mut malformed_cleanup = policy.clone();
        malformed_cleanup["participants"][0]["cleanup_methods"][0]["schema_revision"] = json!(0);
        assert!(parse(malformed_cleanup).is_err());
        let mut duplicate_cleanup = policy.clone();
        duplicate_cleanup["participants"][0]["cleanup_methods"] = json!([method, method]);
        assert!(parse(duplicate_cleanup).is_err());
        let mut missing = policy.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("consumer_requirements");
        assert!(parse(missing).is_err());
        for field in ["capture_providers", "selector_schemas"] {
            let mut empty = policy.clone();
            empty["consumer_requirements"][field] = json!([]);
            assert!(parse(empty).is_err());
            let mut many = policy.clone();
            many["consumer_requirements"][field] = json!(vec!["valid"; 33]);
            assert!(parse(many).is_err());
            let mut invalid = policy.clone();
            invalid["consumer_requirements"][field] = json!(["bad\n"]);
            assert!(parse(invalid).is_err());
        }
        let mut invalid_domain = policy.clone();
        invalid_domain["consumer_requirements"]["execution_domain"] = json!("");
        assert!(parse(invalid_domain).is_err());
        let mut untrusted = policy.clone();
        untrusted["consumers"]["capture"] = json!(["other"]);
        assert!(parse(untrusted).is_err());
        let mut duplicate = policy.clone();
        duplicate["participants"] = json!([participant, participant]);
        assert!(parse(duplicate).is_err());
        let mut invalid_major = policy.clone();
        invalid_major["participants"][0]["plan_method"]["interface_major"] = json!(0);
        assert!(parse(invalid_major.clone()).is_err());
        invalid_major["participants"][0]["plan_method"]["interface_major"] = json!(2);
        assert!(parse(invalid_major.clone()).is_err());
        invalid_major["participants"][0]["contract_version"] = json!(2);
        assert!(parse(invalid_major).is_err());
        let mut unsupported_contract = policy.clone();
        unsupported_contract["participants"][0]["contract_version"] = json!(2);
        assert!(parse(unsupported_contract).is_err());
        let mut invalid_settings = policy.clone();
        invalid_settings["participants"][0]["settings"] = json!([]);
        assert!(parse(invalid_settings).is_err());
        let mut oversize = policy;
        oversize["participants"][0]["settings"] = json!({"data":"a".repeat(8192)});
        assert!(parse(oversize).is_err());
    }
    #[test]
    fn participant_dependencies_resolve_stably_and_reject_invalid_graphs() {
        let method = json!({"service":"provider/1","method":"prepare","interface_major":1,"interface_minor":0,"schema_revision":1});
        let participant = |provider: &str, dependencies: Value| json!({"provider":provider,"depends_on":dependencies,"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"initial","settings":{}});
        let mut settings: LifecycleSettings = serde_json::from_value(json!({"participants":[participant("c",json!(["b"])),participant("independent",json!([])),participant("b",json!(["a"])),participant("a",json!([]))]})).unwrap();
        assert_eq!(
            settings.resolved_participants().unwrap(),
            ["independent", "a", "b", "c"]
        );
        settings.participants[3].depends_on.push("c".into());
        assert!(settings.resolved_participants().is_err());
        settings.participants[3].depends_on = vec!["missing".into()];
        assert!(settings.resolved_participants().is_err());
        settings.participants[3].depends_on = vec!["a".into()];
        assert!(settings.resolved_participants().is_err());
        settings.participants[3].depends_on.clear();
        settings.participants[2].depends_on = vec!["a".into(), "a".into()];
        assert!(settings.resolved_participants().is_err());
        settings.participants[2].depends_on = vec!["a".into()];
        settings.participants[3].provider = "b".into();
        assert!(settings.resolved_participants().is_err());
    }
    #[test]
    fn installed_control_policy_requires_exact_graph_and_valid_pins() {
        let method = json!({"service":"owner/1","method":"apply","interface_major":1,"interface_minor":0,"schema_revision":1});
        let participant = |provider: &str, depends_on: Value| json!({"provider":provider,"depends_on":depends_on,"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"initial","settings":{}});
        use sha2::{Digest, Sha256};
        let document = r#"{"$id":"schema/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#;
        let digest = format!("{:x}", Sha256::digest(document.as_bytes()));
        let schema = json!({"id":"schema/1","major":1,"minor":0,"digest":digest});
        let effect_method = json!({"service":"owner/1","method":"apply","interface_major":2,"interface_minor":0,"schema_revision":1});
        let status_method = json!({"service":"owner/1","method":"status","interface_major":2,"interface_minor":0,"schema_revision":1});
        let policy = json!({"dependent_provider":"dependent","owner_provider":"owner","owner_service":"owner/1","owner_method":effect_method,"status_method":status_method,"settlement_method":effect_method,"argument_schema":schema,"output_capability":"display/output","output_schema":schema,"max_access":"exclusive","mutation_domains":["seat:0"]});
        let mut settings: LifecycleSettings = serde_json::from_value(json!({"journal_path":"/test", "consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"participants":[participant("dependent",json!(["owner"])),participant("owner",json!([]))],"control_policies":[policy], "schema_documents":[{"reference":schema,"document":document}]})).unwrap();
        assert!(settings.validate().is_ok());
        settings
            .control_policies
            .push(settings.control_policies[0].clone());
        assert!(settings.validate().is_err());
        settings.control_policies.pop();
        settings.participants[0].depends_on.clear();
        assert!(settings.validate().is_err());
        settings.participants[0].depends_on.push("owner".into());
        settings.control_policies[0].argument_schema.digest = "A".repeat(64);
        assert!(settings.validate().is_err());
        settings.control_policies[0].argument_schema.digest = digest;
        settings.control_policies[0].owner_method.service = "other/1".into();
        assert!(settings.validate().is_err());
        settings.control_policies[0].owner_method.service = "owner/1".into();
        settings.control_policies[0].max_access = ResourceAccess::Read;
        assert!(settings.validate().is_err());
        settings.control_policies[0].max_access = ResourceAccess::Exclusive;
        settings.control_policies[0].mutation_domains.clear();
        assert!(settings.validate().is_err());
        settings.control_policies[0].domain_scope = DomainScope::Resource;
        assert!(settings.validate().is_ok());
        settings.control_policies[0]
            .mutation_domains
            .insert("seat:0".into());
        assert!(settings.validate().is_err());
        settings.control_policies[0].mutation_domains.clear();
        settings.control_policies[0].status_method.service = "wrong/1".into();
        assert!(settings.validate().is_err());
        settings.control_policies[0].status_method.service = "owner/1".into();
        settings.control_policies[0]
            .settlement_method
            .interface_major = 1;
        assert!(settings.validate().is_err());
        settings.control_policies[0]
            .settlement_method
            .interface_major = 2;
        settings.control_policies[0].settlement_method.service = "wrong/1".into();
        assert!(settings.validate().is_err());
        settings.control_policies[0].settlement_method.service = "owner/1".into();
        settings.control_policies[0]
            .settlement_method
            .schema_revision = 0;
        assert!(settings.validate().is_err());
        settings.control_policies.clear();
        assert!(settings.validate().is_ok());
        let finalize = json!({"owner_provider":"owner","owner_service":"owner/1","method":effect_method,"status_method":status_method,"output_capability":"display/output","output_schema":schema,"capture_provider":"capture","selector_schema":"selector/1","required_access":"exclusive","mutation_domains":["seat:0"]});
        settings.finalize_policies = vec![serde_json::from_value(finalize).unwrap()];
        assert!(settings.validate().is_ok());
        settings
            .finalize_policies
            .push(settings.finalize_policies[0].clone());
        assert!(settings.validate().is_err());
        settings.finalize_policies.pop();
        settings.finalize_policies[0].capture_provider = "wrong".into();
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].capture_provider = "capture".into();
        settings.finalize_policies[0].method.service = "wrong/1".into();
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].method.service = "owner/1".into();
        settings.finalize_policies[0].required_access = ResourceAccess::Read;
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].required_access = ResourceAccess::Exclusive;
        settings.finalize_policies[0].mutation_domains.clear();
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].domain_scope = DomainScope::Resource;
        assert!(settings.validate().is_ok());
        settings.finalize_policies[0]
            .mutation_domains
            .insert("seat:0".into());
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].mutation_domains.clear();
        settings.finalize_policies[0].status_method.interface_major = 1;
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].status_method.interface_major = 2;
        settings.finalize_policies[0].output_schema.digest = "f".repeat(64);
        assert!(settings.validate().is_err());
        settings.finalize_policies.clear();
        assert!(settings.validate().is_ok());
        settings.schema_documents[0].document.push(' ');
        assert!(settings.validate().is_err());
    }
    #[test]
    fn v2_graph_requires_selected_owner_exact_schemas_and_v2_methods() {
        use sha2::{Digest, Sha256};
        let document = r#"{"$id":"output/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#;
        let schema = json!({"id":"output/1","major":1,"minor":0,"digest":format!("{:x}",Sha256::digest(document.as_bytes()))});
        let method = json!({"service":"owner/2","method":"plan","interface_major":2,"interface_minor":0,"schema_revision":1});
        let participant = json!({"provider":"owner","contract_version":2,"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"initial","settings":{}});
        let finalize = json!({"owner_provider":"owner","owner_service":"owner/2","method":method,"status_method":method,"output_capability":"display/output","output_schema":schema,"capture_provider":"capture","selector_schema":"selector/1","required_access":"exclusive","domain_scope":"resource","mutation_domains":[]});
        let settings: LifecycleSettings = serde_json::from_value(json!({"journal_path":"/test","participants":[participant],"finalize_policies":[finalize],"schema_documents":[{"reference":schema,"document":document}],"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]}})).unwrap();
        assert!(settings.validate().is_ok());
        let mut invalid = settings.clone();
        invalid.finalize_policies.clear();
        assert!(invalid.validate().is_err());
        let mut composed = settings.clone();
        let mut dependent = composed.participants[0].clone();
        dependent.provider = "dependent".into();
        dependent.depends_on = vec!["owner".into()];
        for method in [
            &mut dependent.plan_method,
            &mut dependent.callback_method,
            &mut dependent.status_method,
        ] {
            method.service = "dependent/2".into();
        }
        composed.participants.push(dependent);
        let control = json!({"owner_provider":"owner","owner_service":"owner/2","dependent_provider":"dependent","owner_method":method,"status_method":method,"settlement_method":method,"output_capability":"display/output","output_schema":schema,"argument_schema":schema,"max_access":"exclusive","domain_scope":"resource","mutation_domains":[]});
        composed
            .control_policies
            .push(serde_json::from_value(control).unwrap());
        assert!(composed.validate().is_ok());
        let mut invalid = composed.clone();
        invalid.participants[1].depends_on.clear();
        assert!(invalid.validate().is_err());
        let mut invalid = composed.clone();
        invalid.control_policies[0].owner_service = "wrong/2".into();
        assert!(invalid.validate().is_err());
        let mut invalid = composed.clone();
        invalid.control_policies[0].argument_schema.digest = "f".repeat(64);
        assert!(invalid.validate().is_err());
        let mut invalid = composed;
        invalid.participants[1].contract_version = 1;
        invalid.participants[1].plan_method.interface_major = 1;
        invalid.participants[1].callback_method.interface_major = 1;
        invalid.participants[1].status_method.interface_major = 1;
        assert!(invalid.validate().is_err());
        let mut invalid = settings.clone();
        invalid.participants[0].callback_method.interface_major = 1;
        assert!(invalid.validate().is_err());
        let mut invalid = settings.clone();
        invalid.finalize_policies[0].owner_provider = "other".into();
        assert!(invalid.validate().is_err());
        let mut invalid = settings.clone();
        invalid.schema_documents.clear();
        assert!(invalid.validate().is_err());
        let mut invalid = settings.clone();
        invalid.participants[0].depends_on.push("missing".into());
        assert!(invalid.validate().is_err());
        let mut invalid = settings;
        invalid.participants[0].contract_version = 3;
        assert!(invalid.validate().is_err());
    }
}
