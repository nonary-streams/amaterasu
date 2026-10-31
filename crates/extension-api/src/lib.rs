//! Serialized extension contracts. References select resources; they never confer authority.
mod action_target;
mod composition;
pub mod schemas;
pub use action_target::*;
pub use composition::*;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SESSION_LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";
pub const EXTENSION_REGISTRY_SERVICE: &str = "amaterasu.extension-registry/1";
pub const EXTENSION_ACTIONS_SERVICE: &str = "amaterasu.extension-actions/1";
pub const CONTRACT_VERSION: u32 = 1;
pub const COMPOSITION_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub hdr: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRef {
    pub provider: String,
    pub resource_id: String,
    /// Native producer lifetime, shared by its consumer borrows. This is not a
    /// consumer's native lease token; the selected provider retains those.
    pub incarnation: String,
    pub revision: u64,
    pub execution_domain: String,
    pub mutation_domain: Option<String>,
    #[serde(default)]
    pub mutation_domains: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureBinding {
    pub capture_provider: String,
    pub selector_schema: String,
    pub selector: Value,
    pub resource: ResourceRef,
    pub mode: VideoMode,
    /// The admitted plan revision, preserved across its native effect steps.
    pub plan_revision: u64,
    /// The selected provider's verified geometry observation. It may become
    /// known only after an admitted effect; the coordinator freezes the exact
    /// published value together with this binding and stream generation.
    pub geometry_revision: u64,
    pub generation: u64,
    #[serde(default)]
    pub fallback: CaptureFallback,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFallback {
    #[default]
    Forbidden,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LifecycleTarget {
    Session {
        host_epoch: String,
        session_id: String,
        generation: u64,
    },
    Stream {
        host_epoch: String,
        session_id: String,
        stream_id: String,
        generation: u64,
    },
    Validation {
        host_epoch: String,
        validation_id: String,
        generation: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionTarget {
    Session {
        host_epoch: String,
        session_id: String,
        expected_generation: u64,
    },
    Stream {
        host_epoch: String,
        session_id: String,
        stream_id: String,
        expected_generation: u64,
    },
    Host {
        host_epoch: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionInvoke {
    pub epoch: String,
    pub sequence: u64,
    pub operation_id: String,
    pub action_id: String,
    pub target: ActionTarget,
    pub arguments: Value,
    pub expected_settings_revision: Option<String>,
    /// Opaque selector for a lifecycle-owned reservation, never authority by itself.
    #[serde(default)]
    pub input_reservation_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    /// Input is reserved; action execution has not yet been admitted.
    Reserved,
    Queued,
    Running,
    Succeeded,
    Rejected,
    Cancelled,
    Failed,
    RecoveryRequired,
    ReceiptExpired,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReceipt {
    pub receipt_id: String,
    pub operation_id: String,
    pub status: ActionStatus,
    pub result: Option<Value>,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    Host,
    Application,
    PairedClient,
    Session,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPolicy {
    NextSession,
    NextStream,
    LiveAction,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClass {
    ReadOnly,
    SessionMutation,
    HostMutation,
}

/// A provider-owned cleanup obligation retained separately from live handles.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceObligation {
    pub obligation_id: String,
    pub provider: String,
    pub resource_id: String,
    pub mutation_domain: Option<String>,
    pub depends_on: Vec<String>,
    pub cleanup_method: MethodRef,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAccess {
    Read,
    Shared,
    Exclusive,
}

/// Expected shared-domain revision and borrowers are rechecked at effect time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationDomainPlan {
    pub domain: String,
    pub expected_revision: u64,
    pub borrowers: std::collections::BTreeMap<String, u64>,
    pub access: ResourceAccess,
}

/// A plan may name a resource before its native incarnation exists. Refinement
/// must stay within the selected provider, logical ID and execution domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedResource {
    pub provider: String,
    pub resource_id: String,
    pub incarnation: Option<String>,
    pub expected_revision: Option<u64>,
    pub execution_domain: String,
    pub mutation_domains: Vec<String>,
    pub access: ResourceAccess,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePlan {
    pub plan_id: String,
    pub revision: u64,
    pub resources: Vec<PlannedResource>,
    pub mutation_domains: Vec<MutationDomainPlan>,
    pub required_capabilities: Vec<String>,
    /// Provider-specific constraints interpreted under an approved schema.
    pub constraints: Value,
    pub obligations: Vec<ResourceObligation>,
    pub affected_consumers: Vec<String>,
    pub consumer_change: ConsumerChange,
    pub candidate_constraint: Option<BindingConstraint>,
    pub allowed_degradation: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control_proposals: Vec<ResourceControlProposal>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsumerChange {
    Live,
    RestartPipeline,
    ReconnectRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingConstraint {
    pub capture_provider: String,
    pub selector_schema: String,
    pub resource_provider: String,
    pub resource_id: String,
    pub incarnation: Option<String>,
    pub execution_domain: String,
    pub mode: Option<VideoMode>,
    pub plan_revision: u64,
    /// Frozen geometry observation for candidate validation; absent in v1 plans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderStatusRequest {
    pub provider_sequence: u64,
    pub operation_id: String,
    pub transition_id: String,
    pub receipt_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupState {
    Released,
    Pending,
    DeferredRestoration,
    EffectUnknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupOutcome {
    pub obligation_id: String,
    pub state: CleanupState,
    pub reason: Option<String>,
}

/// Coordinator-selected consumer constraints established before provider planning.
/// These fields constrain a plan; they never authorize the requester.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerRequirements {
    pub execution_domain: String,
    pub capture_providers: Vec<String>,
    pub selector_schemas: Vec<String>,
}

/// Calls use the coordinator's broker identity after durable admission. The
/// admission ID identifies a recorded plan; the ID alone is not authority.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRequest {
    /// Persisted monotonic sequence per provider and authenticated coordinator.
    pub provider_sequence: u64,
    pub operation: ProviderOperation,
    pub version: u32,
    pub transition_id: String,
    pub operation_id: String,
    pub target: LifecycleTarget,
    pub phase: LifecyclePhase,
    pub settings_revision: String,
    pub settings: amaterasu_configuration_schema::Values,
    pub intent: Option<StreamIntent>,
    pub reason: Option<ReleaseReason>,
    pub requested_video: Option<VideoMode>,
    pub consumer_requirements: ConsumerRequirements,
    pub remaining_ms: u64,
    pub plan_revision: Option<u64>,
    pub admission_id: Option<String>,
    pub approved_plan: Option<ResourcePlan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_inputs: Vec<ResourceOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub control_results: Vec<ConfirmedControl>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOperation {
    ResourcePlan,
    /// Read-only v2 planning for finalizing a staged owner output.
    ResourceFinalizePlan,
    LifecycleCallback,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecyclePhase {
    #[serde(rename = "session.prepare")]
    SessionPrepare,
    #[serde(rename = "stream.prepare")]
    StreamPrepare,
    #[serde(rename = "stream.release")]
    StreamRelease,
    #[serde(rename = "session.release")]
    SessionRelease,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamIntent {
    Initial,
    Resume,
    Reconfigure,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    Pause,
    End,
    FailedStart,
    Reconfigure,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderResponse {
    Planned {
        plan: ResourcePlan,
    },
    Prepared {
        candidate: Option<CaptureBinding>,
        obligations: Vec<ResourceObligation>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        outputs: Vec<ResourceOutput>,
    },
    Pending {
        receipt_id: String,
    },
    Rejected {
        reason: String,
    },
    Cleanup {
        outcomes: Vec<CleanupOutcome>,
    },
    Failed {
        reason: String,
    },
    RecoveryRequired {
        reason: String,
    },
}

/// An explicit editor hint; consumers never infer keybindings from object shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionPresentation {
    Keybinding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum KeyIdentity {
    LinuxEvdev { code: u16 },
    WindowsScan { code: u16, extended: bool },
    MoonlightVk { code: u8, normalized: bool },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingSource {
    SessionInput,
    HostLocal,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingPropagation {
    ConsumeTrigger,
    Forward,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingTarget {
    CurrentSession,
    Host,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keybinding {
    pub id: String,
    pub enabled: bool,
    pub source: BindingSource,
    /// Opaque provider-scoped context, never an authority claim.
    pub context: String,
    pub trigger: KeyIdentity,
    pub modifiers: Vec<KeyIdentity>,
    pub propagation: BindingPropagation,
    pub action_id: String,
    pub arguments: amaterasu_configuration_schema::Values,
    pub target: BindingTarget,
}

/// Validate shape and action arguments; authorization and chord conflicts are separate.
/// The selected input adapter must also validate platform-specific key-code support.
pub fn validate_keybinding(binding: &Keybinding, action: &ActionDescriptor) -> Result<(), String> {
    if !valid_opaque_id(&binding.id)
        || !valid_opaque_id(&binding.context)
        || binding.action_id != action.id
        || binding.modifiers.len() > 8
        || binding
            .modifiers
            .iter()
            .any(|key| std::mem::discriminant(key) != std::mem::discriminant(&binding.trigger))
        || binding.modifiers.contains(&binding.trigger)
        || binding
            .modifiers
            .iter()
            .enumerate()
            .any(|(index, key)| binding.modifiers[..index].contains(key))
    {
        return Err("invalid keybinding".into());
    }
    amaterasu_configuration_schema::validate_values(&action.arguments, &binding.arguments)
}

/// A registered method is selected only when its installed approval matches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodRef {
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub schema_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Session,
    Stream,
    Host,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionDescriptor {
    pub id: String,
    pub label: String,
    pub help: String,
    pub definition: amaterasu_configuration_schema::Definition,
    #[serde(default)]
    pub presentation: Option<OptionPresentation>,
    pub allowed_scopes: Vec<SettingScope>,
    pub apply_policy: ApplyPolicy,
    pub required_capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDescriptor {
    pub id: String,
    pub label: String,
    pub description: String,
    pub execution: ExecutionClass,
    pub arguments: amaterasu_configuration_schema::Fields,
    pub allowed_targets: Vec<TargetKind>,
    pub required_capabilities: Vec<String>,
    pub plan: Option<MethodRef>,
    pub execute: MethodRef,
    pub status: Option<MethodRef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDescriptor {
    pub provider: String,
    pub revision: u64,
    pub options: Vec<OptionDescriptor>,
    pub actions: Vec<ActionDescriptor>,
    pub capabilities: Vec<String>,
    pub settings_validate: MethodRef,
}

/// Check a provider request's shape before processing it. The broker caller,
/// admitted plan and installed grants must still be checked by the coordinator.
pub fn validate_provider_request(request: &ProviderRequest) -> Result<(), String> {
    if request.provider_sequence == 0
        || !matches!(request.version, CONTRACT_VERSION | COMPOSITION_VERSION)
        || !valid_opaque_id(&request.transition_id)
        || !valid_opaque_id(&request.operation_id)
        || !valid_opaque_id(&request.settings_revision)
        || !valid_opaque_id(&request.consumer_requirements.execution_domain)
        || request.consumer_requirements.capture_providers.is_empty()
        || request.consumer_requirements.capture_providers.len() > 32
        || request
            .consumer_requirements
            .capture_providers
            .iter()
            .any(|id| !valid_identifier(id))
        || request.consumer_requirements.selector_schemas.is_empty()
        || request.consumer_requirements.selector_schemas.len() > 32
        || request
            .consumer_requirements
            .selector_schemas
            .iter()
            .any(|id| !valid_identifier(id))
        || request.remaining_ms == 0
        || serde_json::to_vec(request)
            .map_err(|error| error.to_string())?
            .len()
            > 12 * 1024
    {
        return Err("invalid provider request".into());
    }
    if request.dependency_inputs.len() > 32
        || request.control_results.len() > 32
        || (request.version == CONTRACT_VERSION
            && (!request.dependency_inputs.is_empty() || !request.control_results.is_empty()))
    {
        return Err("invalid composition fields".into());
    }
    let mut selected = std::collections::BTreeSet::new();
    for input in &request.dependency_inputs {
        validate_resource_output(input)?;
        if !output_in_scope(input, &request.target)
            || input.resource.execution_domain != request.consumer_requirements.execution_domain
            || !selected.insert((&input.producer, &input.output_id))
        {
            return Err("invalid dependency input".into());
        }
    }
    match request.operation {
        ProviderOperation::ResourcePlan => {
            if request.approved_plan.is_some()
                || request.admission_id.is_some()
                || !request.control_results.is_empty()
            {
                return Err("planning does not authorize effects".into());
            }
        }
        ProviderOperation::ResourceFinalizePlan => {
            if request.version != COMPOSITION_VERSION
                || request.dependency_inputs.len() != 1
                || request.approved_plan.is_some()
                || request.admission_id.is_some()
                || request.plan_revision.is_some()
            {
                return Err("invalid finalize planning request".into());
            }
            let current = &request.dependency_inputs[0];
            let mut seen = std::collections::BTreeSet::new();
            for result in &request.control_results {
                if !valid_opaque_id(&result.proposal_id) || !seen.insert(&result.proposal_id) {
                    return Err("invalid finalize control result".into());
                }
                validate_output_refinement(&result.output, current)?;
                if let Some(candidate) = &result.candidate {
                    validate_capture_binding(candidate)?;
                    if candidate.resource != result.output.resource
                        || !matches!(&request.target, LifecycleTarget::Stream { generation, .. } if candidate.generation == *generation)
                    {
                        return Err("finalize control candidate mismatch".into());
                    }
                }
            }
        }
        ProviderOperation::LifecycleCallback => {
            let plan = request
                .approved_plan
                .as_ref()
                .ok_or("missing admitted plan")?;
            if !request.admission_id.as_deref().is_some_and(valid_opaque_id)
                || request.plan_revision != Some(plan.revision)
            {
                return Err("missing or mismatched admission".into());
            }
            validate_resource_plan(plan)?;
            validate_composition_plan(request, plan)?;
            validate_composition_results(request, plan)?;
            validate_consumer_requirements(plan, &request.consumer_requirements)?;
        }
    }
    Ok(())
}

/// Validate structural plan bounds and dependency identities. The coordinator
/// must separately compare observed revisions, borrowers and installed grants.
pub fn validate_resource_plan(plan: &ResourcePlan) -> Result<(), String> {
    use std::collections::BTreeSet;
    if !valid_opaque_id(&plan.plan_id)
        || plan.revision == 0
        || plan.resources.len() > 128
        || plan.mutation_domains.len() > 128
        || plan.obligations.len() > 128
        || plan.affected_consumers.len() > 128
        || plan.required_capabilities.len() > 128
        || plan.allowed_degradation.len() > 128
        || plan.control_proposals.len() > 32
        || !plan.constraints.is_object()
        || serde_json::to_vec(plan)
            .map_err(|error| error.to_string())?
            .len()
            > 12 * 1024
    {
        return Err("invalid resource plan".into());
    }
    let mut domains = BTreeSet::new();
    for domain in &plan.mutation_domains {
        if !valid_opaque_id(&domain.domain)
            || !domains.insert(&domain.domain)
            || domain.borrowers.len() > 128
            || domain.borrowers.keys().any(|id| !valid_opaque_id(id))
        {
            return Err("invalid mutation domain".into());
        }
    }
    let mut resources = BTreeSet::new();
    for resource in &plan.resources {
        let resource_domains: BTreeSet<_> = resource.mutation_domains.iter().collect();
        if !valid_identifier(&resource.provider)
            || !valid_opaque_id(&resource.resource_id)
            || !valid_opaque_id(&resource.execution_domain)
            || resource
                .incarnation
                .as_ref()
                .is_some_and(|id| !valid_opaque_id(id))
            || !resources.insert((&resource.provider, &resource.resource_id))
            || resource_domains.len() != resource.mutation_domains.len()
            || resource
                .mutation_domains
                .iter()
                .any(|domain| !domains.contains(domain))
        {
            return Err("invalid planned resource".into());
        }
    }
    let mut obligations = BTreeSet::new();
    for obligation in &plan.obligations {
        if !valid_opaque_id(&obligation.obligation_id)
            || !obligations.insert(&obligation.obligation_id)
            || !valid_identifier(&obligation.provider)
            || !valid_opaque_id(&obligation.resource_id)
            || !obligation.payload.is_object()
            || obligation.depends_on.len() > 128
        {
            return Err("invalid resource obligation".into());
        }
        validate_method(&obligation.cleanup_method)?;
    }
    for obligation in &plan.obligations {
        if obligation.depends_on.iter().any(|dependency| {
            dependency == &obligation.obligation_id || !obligations.contains(dependency)
        }) {
            return Err("invalid obligation dependency".into());
        }
    }
    if let Some(binding) = &plan.candidate_constraint
        && (!valid_identifier(&binding.capture_provider)
            || !valid_identifier(&binding.selector_schema)
            || !valid_identifier(&binding.resource_provider)
            || !valid_opaque_id(&binding.resource_id)
            || !valid_opaque_id(&binding.execution_domain)
            || binding.plan_revision != plan.revision
            || binding.geometry_revision == Some(0)
            || binding
                .incarnation
                .as_ref()
                .is_some_and(|id| !valid_opaque_id(id))
            || !plan.resources.iter().any(|resource| {
                resource.provider == binding.resource_provider
                    && resource.resource_id == binding.resource_id
                    && resource.execution_domain == binding.execution_domain
                    && resource
                        .incarnation
                        .as_ref()
                        .is_none_or(|incarnation| binding.incarnation.as_ref() == Some(incarnation))
            }))
    {
        return Err("invalid binding constraint".into());
    }
    let mut proposals = BTreeSet::new();
    for proposal in &plan.control_proposals {
        validate_resource_control_proposal(proposal)?;
        if !proposals.insert(&proposal.proposal_id)
            || proposal.mutation_domains.iter().any(|domain| {
                !plan.mutation_domains.iter().any(|declared| {
                    declared.domain == *domain
                        && matches!(
                            (&declared.access, &proposal.access),
                            (ResourceAccess::Exclusive, _)
                                | (
                                    ResourceAccess::Shared,
                                    ResourceAccess::Shared | ResourceAccess::Read
                                )
                                | (ResourceAccess::Read, ResourceAccess::Read)
                        )
                })
            })
        {
            return Err("invalid control proposal".into());
        }
    }
    let mut retired = BTreeSet::new();
    loop {
        let previous = retired.len();
        for obligation in &plan.obligations {
            if obligation
                .depends_on
                .iter()
                .all(|dependency| retired.contains(dependency))
            {
                retired.insert(obligation.obligation_id.clone());
            }
        }
        if retired.len() == plan.obligations.len() {
            break;
        }
        if retired.len() == previous {
            return Err("cyclic obligation dependency".into());
        }
    }
    Ok(())
}

fn validate_composition_plan(request: &ProviderRequest, plan: &ResourcePlan) -> Result<(), String> {
    if request.version == CONTRACT_VERSION && !plan.control_proposals.is_empty() {
        return Err("v1 control proposals unavailable".into());
    }
    for proposal in &plan.control_proposals {
        if proposal.target != request.target
            || !request.dependency_inputs.iter().any(|input| {
                input.output_id == proposal.output_id
                    && input.producer == proposal.owner_provider
                    && input.producer_service == proposal.owner_service
                    && input.resource == proposal.resource
            })
        {
            return Err("control proposal outside selected inputs".into());
        }
    }
    Ok(())
}

fn validate_composition_results(
    request: &ProviderRequest,
    plan: &ResourcePlan,
) -> Result<(), String> {
    if request.version == COMPOSITION_VERSION
        && request.control_results.len() != plan.control_proposals.len()
    {
        return Err("control results incomplete".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for result in &request.control_results {
        if !seen.insert(&result.proposal_id) {
            return Err("duplicate control result".into());
        }
        let proposal = plan
            .control_proposals
            .iter()
            .find(|p| p.proposal_id == result.proposal_id)
            .ok_or("unadmitted control result")?;
        validate_resource_output(&result.output)?;
        let original = request
            .dependency_inputs
            .iter()
            .find(|input| {
                input.output_id == proposal.output_id && input.producer == proposal.owner_provider
            })
            .ok_or("missing original input")?;
        validate_output_refinement(original, &result.output)?;
        if result.output.output_id != original.output_id
            || result.output.producer != original.producer
            || result.output.producer_service != original.producer_service
            || result.output.origin != original.origin
            || result.output.schema != original.schema
            || result.output.capability != original.capability
            || result.output.resource.provider != original.resource.provider
            || result.output.resource.resource_id != original.resource.resource_id
            || result.output.resource.incarnation != original.resource.incarnation
            || result.output.resource.execution_domain != original.resource.execution_domain
            || result.output.resource.mutation_domain != original.resource.mutation_domain
            || result.output.resource.mutation_domains != original.resource.mutation_domains
            || result.output.resource.revision < original.resource.revision
        {
            return Err("control result outside admitted resource".into());
        }
        if let Some(candidate) = &result.candidate {
            validate_capture_binding(candidate)?;
            if candidate.resource != result.output.resource
                || !matches!(&request.target, LifecycleTarget::Stream { generation, .. } if candidate.generation == *generation)
            {
                return Err("control candidate mismatch".into());
            }
        }
    }
    Ok(())
}

fn validate_consumer_requirements(
    plan: &ResourcePlan,
    requirements: &ConsumerRequirements,
) -> Result<(), String> {
    if let Some(binding) = &plan.candidate_constraint
        && (binding.execution_domain != requirements.execution_domain
            || !requirements
                .capture_providers
                .contains(&binding.capture_provider)
            || !requirements
                .selector_schemas
                .contains(&binding.selector_schema))
    {
        return Err("binding violates selected consumer requirements".into());
    }
    Ok(())
}

/// Check response shape and ensure a prepared binding respects its admitted plan.
/// This does not attest that a capture consumer actually opened that resource.
pub fn validate_provider_response(
    request: &ProviderRequest,
    response: &ProviderResponse,
) -> Result<(), String> {
    validate_provider_request(request)?;
    if serde_json::to_vec(response)
        .map_err(|error| error.to_string())?
        .len()
        > 12 * 1024
    {
        return Err("provider response exceeds size limit".into());
    }
    match response {
        ProviderResponse::Planned { plan }
            if request.operation == ProviderOperation::ResourcePlan =>
        {
            validate_resource_plan(plan)?;
            validate_composition_plan(request, plan)?;
            validate_consumer_requirements(plan, &request.consumer_requirements)
        }
        ProviderResponse::Planned { plan }
            if request.operation == ProviderOperation::ResourceFinalizePlan =>
        {
            validate_resource_plan(plan)?;
            validate_consumer_requirements(plan, &request.consumer_requirements)?;
            let output = &request.dependency_inputs[0];
            if !plan.control_proposals.is_empty()
                || plan
                    .candidate_constraint
                    .as_ref()
                    .is_some_and(|constraint| constraint.geometry_revision.is_none())
                || !plan.resources.iter().any(|resource| {
                    resource.provider == output.resource.provider
                        && resource.resource_id == output.resource.resource_id
                        && resource.incarnation.as_deref() == Some(&output.resource.incarnation)
                        && resource.expected_revision == Some(output.resource.revision)
                        && resource.execution_domain == output.resource.execution_domain
                        && resource
                            .mutation_domains
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            == output
                                .domain_observations
                                .iter()
                                .map(|entry| &entry.domain)
                                .collect()
                })
                || output.domain_observations.iter().any(|observed| {
                    !plan.mutation_domains.iter().any(|domain| {
                        domain.domain == observed.domain
                            && domain.expected_revision == observed.revision
                            && domain.borrowers == observed.borrowers
                    })
                })
            {
                return Err("finalize plan outside selected output".into());
            }
            Ok(())
        }
        ProviderResponse::Prepared {
            candidate,
            obligations,
            outputs,
        } if request.operation == ProviderOperation::LifecycleCallback => {
            let plan = request
                .approved_plan
                .as_ref()
                .ok_or("missing admitted plan")?;
            if outputs.len() > 32
                || (request.version == CONTRACT_VERSION && !outputs.is_empty())
                || (request.version == COMPOSITION_VERSION && candidate.is_some())
            {
                return Err("invalid prepared outputs".into());
            }
            let mut output_ids = std::collections::BTreeSet::new();
            for output in outputs {
                validate_resource_output(output)?;
                let output_domains: std::collections::BTreeSet<_> = output
                    .resource
                    .mutation_domains
                    .iter()
                    .chain(output.resource.mutation_domain.iter())
                    .collect();
                if !output_in_scope(output, &request.target)
                    || !output_ids.insert((&output.producer, &output.output_id))
                    || !plan.resources.iter().any(|resource| {
                        resource.provider == output.resource.provider
                            && resource.resource_id == output.resource.resource_id
                            && resource.execution_domain == output.resource.execution_domain
                            && resource
                                .mutation_domains
                                .iter()
                                .collect::<std::collections::BTreeSet<_>>()
                                == output_domains
                            && resource
                                .incarnation
                                .as_ref()
                                .is_none_or(|id| id == &output.resource.incarnation)
                            && resource
                                .expected_revision
                                .is_none_or(|revision| revision == output.resource.revision)
                    })
                {
                    return Err("output outside admitted plan".into());
                }
            }
            if obligations
                .iter()
                .any(|obligation| !plan.obligations.contains(obligation))
            {
                return Err("unadmitted obligation".into());
            }
            if let Some(candidate) = candidate {
                validate_capture_binding(candidate)?;
                let constraint = plan
                    .candidate_constraint
                    .as_ref()
                    .ok_or("unadmitted binding")?;
                if candidate.capture_provider != constraint.capture_provider
                    || candidate.selector_schema != constraint.selector_schema
                    || candidate.resource.provider != constraint.resource_provider
                    || candidate.resource.resource_id != constraint.resource_id
                    || candidate.resource.execution_domain != constraint.execution_domain
                    || candidate.plan_revision != constraint.plan_revision
                    || constraint
                        .geometry_revision
                        .is_some_and(|revision| candidate.geometry_revision != revision)
                    || constraint
                        .incarnation
                        .as_ref()
                        .is_some_and(|incarnation| incarnation != &candidate.resource.incarnation)
                    || constraint
                        .mode
                        .as_ref()
                        .is_some_and(|mode| mode != &candidate.mode)
                {
                    return Err("binding outside admitted plan".into());
                }
            }
            Ok(())
        }
        ProviderResponse::Pending { receipt_id } if valid_opaque_id(receipt_id) => Ok(()),
        ProviderResponse::Rejected { reason }
        | ProviderResponse::Failed { reason }
        | ProviderResponse::RecoveryRequired { reason }
            if !reason.is_empty() && reason.len() <= 4096 =>
        {
            Ok(())
        }
        ProviderResponse::Cleanup { outcomes }
            if request.operation == ProviderOperation::LifecycleCallback =>
        {
            let plan = request
                .approved_plan
                .as_ref()
                .ok_or("missing admitted plan")?;
            let mut seen = std::collections::BTreeSet::new();
            if outcomes.iter().any(|outcome| {
                !seen.insert(&outcome.obligation_id)
                    || !plan
                        .obligations
                        .iter()
                        .any(|obligation| obligation.obligation_id == outcome.obligation_id)
                    || outcome
                        .reason
                        .as_ref()
                        .is_some_and(|reason| reason.len() > 4096)
            }) {
                return Err("invalid cleanup outcome".into());
            }
            Ok(())
        }
        _ => Err("response does not match provider operation".into()),
    }
}

/// Validate an exact candidate before any consumer opens it. Authorization,
/// provider availability, and actual capture acknowledgement are separate checks.
pub fn validate_capture_binding(binding: &CaptureBinding) -> Result<(), String> {
    let resource = &binding.resource;
    if !valid_identifier(&binding.capture_provider)
        || !valid_identifier(&binding.selector_schema)
        || !valid_identifier(&resource.provider)
        || !valid_opaque_id(&resource.resource_id)
        || !valid_opaque_id(&resource.incarnation)
        || !valid_opaque_id(&resource.execution_domain)
        || resource
            .mutation_domain
            .as_ref()
            .is_some_and(|domain| !valid_opaque_id(domain))
        || !binding.selector.is_object()
        || resource
            .mutation_domains
            .iter()
            .any(|domain| !valid_opaque_id(domain))
        || resource.revision == 0
        || binding.plan_revision == 0
        || binding.geometry_revision == 0
        || binding.generation == 0
        || binding.mode.width == 0
        || binding.mode.height == 0
        || binding.mode.refresh_millihz == 0
        || serde_json::to_vec(binding)
            .map_err(|error| error.to_string())?
            .len()
            > 12 * 1024
    {
        return Err("invalid capture binding".into());
    }
    Ok(())
}

/// Validate a descriptor's structural bounds before installation or discovery.
/// Installed approvals and caller authorization must be checked separately.
pub fn validate_descriptor(descriptor: &ExtensionDescriptor) -> Result<(), String> {
    use std::collections::BTreeSet;
    const MAX_DESCRIPTOR_BYTES: usize = 12 * 1024;
    if serde_json::to_vec(descriptor)
        .map_err(|e| e.to_string())?
        .len()
        > MAX_DESCRIPTOR_BYTES
    {
        return Err("descriptor exceeds size limit".into());
    }
    if !valid_identifier(&descriptor.provider)
        || descriptor.options.len() > 128
        || descriptor.actions.len() > 128
        || descriptor.capabilities.len() > 128
    {
        return Err("invalid provider or descriptor count".into());
    }
    validate_method(&descriptor.settings_validate)?;
    let mut options = BTreeSet::new();
    for option in &descriptor.options {
        if !valid_identifier(&option.id)
            || !options.insert(&option.id)
            || option.allowed_scopes.is_empty()
            || option.label.len() > 256
            || option.help.len() > 4096
        {
            return Err("invalid option descriptor".into());
        }
        let fields = [(option.id.clone(), option.definition.clone())]
            .into_iter()
            .collect();
        amaterasu_configuration_schema::defaults(&fields)?;
    }
    let mut actions = BTreeSet::new();
    for action in &descriptor.actions {
        if !valid_identifier(&action.id)
            || !action.id.starts_with(&format!("{}/", descriptor.provider))
            || !actions.insert(&action.id)
            || action.allowed_targets.is_empty()
            || action.label.len() > 256
            || action.description.len() > 4096
        {
            return Err("invalid action descriptor".into());
        }
        if action.execution != ExecutionClass::ReadOnly && action.plan.is_none() {
            return Err("mutating action requires a plan method".into());
        }
        amaterasu_configuration_schema::defaults(&action.arguments)?;
        validate_method(&action.execute)?;
        if let Some(method) = &action.plan {
            validate_method(method)?;
        }
        if let Some(method) = &action.status {
            validate_method(method)?;
        }
    }
    if descriptor
        .capabilities
        .iter()
        .any(|capability| !valid_identifier(capability))
    {
        return Err("invalid capability".into());
    }
    Ok(())
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

fn validate_method(method: &MethodRef) -> Result<(), String> {
    if !valid_identifier(&method.service)
        || !valid_identifier(&method.method)
        || method.interface_major == 0
        || method.schema_revision == 0
    {
        return Err("invalid method reference".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn v1_omits_composition_fields_and_v2_binds_selected_input() {
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":2});
        let origin =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
        let schema = json!({"id":"example.schema/1","major":1,"minor":0,"digest":"a".repeat(64)});
        let resource = json!({"provider":"example.owner","resource_id":"resource","incarnation":"instance","revision":4,"execution_domain":"seat","mutation_domain":"domain","mutation_domains":["domain"]});
        let output = json!({"output_id":"selected","capability":"example.target","schema":schema,"producer":"example.owner","producer_service":"example.owner/2","origin":origin,"resource":resource,"domain_observations":[{"domain":"domain","revision":1,"borrowers":{},"complete":true}],"data":{}});
        let mut request: ProviderRequest = serde_json::from_value(json!({"version":1,"provider_sequence":1,"operation":"resource_plan","transition_id":"transition","operation_id":"operation","target":target,"phase":"stream.prepare","settings_revision":"settings","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null})).unwrap();
        assert!(validate_provider_request(&request).is_ok());
        assert!(
            serde_json::to_value(&request)
                .unwrap()
                .get("dependency_inputs")
                .is_none()
        );
        request
            .dependency_inputs
            .push(serde_json::from_value(output.clone()).unwrap());
        assert!(validate_provider_request(&request).is_err());
        request.version = 2;
        assert!(validate_provider_request(&request).is_ok());
        let proposal = json!({"proposal_id":"change","output_id":"selected","owner_provider":"example.owner","owner_service":"example.owner/2","target":target,"resource":resource,"method":{"service":"example.owner/2","method":"resource_control","interface_major":2,"interface_minor":0,"schema_revision":1},"argument_schema":schema,"arguments":{},"access":"exclusive","mutation_domains":["domain"]});
        let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[{"domain":"domain","expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[],"control_proposals":[proposal]})).unwrap();
        assert!(
            validate_provider_response(&request, &ProviderResponse::Planned { plan: plan.clone() })
                .is_ok()
        );
        let mut foreign = request.clone();
        foreign.dependency_inputs[0].resource.incarnation = "other".into();
        assert!(
            validate_provider_response(&foreign, &ProviderResponse::Planned { plan: plan.clone() })
                .is_err()
        );
        request.operation = ProviderOperation::LifecycleCallback;
        request.admission_id = Some("admitted".into());
        request.plan_revision = Some(1);
        request.approved_plan = Some(plan);
        assert!(validate_provider_request(&request).is_err());
        request.control_results.push(ConfirmedControl {
            proposal_id: "change".into(),
            output: serde_json::from_value(output).unwrap(),
            candidate: None,
        });
        assert!(validate_provider_request(&request).is_ok());
    }

    #[test]
    fn prepared_output_domains_must_equal_admitted_resource_domains() {
        let target =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
        let resource = json!({"provider":"example.owner","resource_id":"resource","incarnation":"instance","revision":4,"execution_domain":"seat","mutation_domain":"domain","mutation_domains":["domain"]});
        let output: ResourceOutput = serde_json::from_value(json!({"output_id":"selected","capability":"example.target","schema":{"id":"example.schema/1","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"example.owner","producer_service":"example.owner/2","origin":target,"resource":resource,"domain_observations":[{"domain":"domain","revision":1,"borrowers":{},"complete":true}],"data":{}})).unwrap();
        let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[{"provider":"example.owner","resource_id":"resource","incarnation":"instance","expected_revision":4,"execution_domain":"seat","mutation_domains":["domain"],"access":"exclusive"}],"mutation_domains":[{"domain":"domain","expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let request: ProviderRequest = serde_json::from_value(json!({"version":2,"provider_sequence":1,"operation":"lifecycle_callback","transition_id":"transition","operation_id":"operation","target":target,"phase":"session.prepare","settings_revision":"settings","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":5000,"plan_revision":1,"admission_id":"admitted","approved_plan":plan})).unwrap();
        let response = |output| ProviderResponse::Prepared {
            candidate: None,
            obligations: vec![],
            outputs: vec![output],
        };
        assert!(validate_provider_response(&request, &response(output.clone())).is_ok());
        let mut foreign = output.clone();
        foreign.resource.mutation_domains.push("foreign".into());
        foreign.domain_observations.push(DomainObservation {
            domain: "foreign".into(),
            revision: 1,
            borrowers: Default::default(),
            complete: true,
        });
        assert!(validate_resource_output(&foreign).is_ok());
        assert!(validate_provider_response(&request, &response(foreign)).is_err());
        let mut omitted = output;
        omitted.resource.mutation_domain = None;
        omitted.resource.mutation_domains.clear();
        omitted.domain_observations.clear();
        assert!(validate_provider_response(&request, &response(omitted)).is_err());
        let mut duplicate_plan = request.approved_plan.unwrap();
        duplicate_plan.resources[0]
            .mutation_domains
            .push("domain".into());
        assert!(validate_resource_plan(&duplicate_plan).is_err());
    }

    #[test]
    fn finalize_planning_binds_current_output_and_prior_controls() {
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":2});
        let resource = json!({"provider":"example.owner","resource_id":"resource","incarnation":"instance","revision":4,"execution_domain":"seat","mutation_domain":"domain","mutation_domains":["domain"]});
        let output = json!({"output_id":"selected","capability":"example.target","schema":{"id":"example.schema/1","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"example.owner","producer_service":"example.owner/2","origin":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1},"resource":resource,"domain_observations":[{"domain":"domain","revision":3,"borrowers":{"stream":2},"complete":true}],"data":{}});
        let request: ProviderRequest = serde_json::from_value(json!({"version":2,"provider_sequence":1,"operation":"resource_finalize_plan","transition_id":"transition","operation_id":"operation","target":target,"phase":"stream.prepare","settings_revision":"settings","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null,"dependency_inputs":[output],"control_results":[{"proposal_id":"control","output":output,"candidate":null}]})).unwrap();
        let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"final-plan","revision":1,"resources":[{"provider":"example.owner","resource_id":"resource","incarnation":"instance","expected_revision":4,"execution_domain":"seat","mutation_domains":["domain"],"access":"read"}],"mutation_domains":[{"domain":"domain","expected_revision":3,"borrowers":{"stream":2},"access":"read"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        assert!(validate_provider_request(&request).is_ok());
        assert!(
            validate_provider_response(&request, &ProviderResponse::Planned { plan: plan.clone() })
                .is_ok()
        );
        let mut old = request.clone();
        old.version = CONTRACT_VERSION;
        assert!(validate_provider_request(&old).is_err());
        let mut extra = request.clone();
        extra.admission_id = Some("admission".into());
        assert!(validate_provider_request(&extra).is_err());
        let mut extra = request.clone();
        extra.dependency_inputs.clear();
        assert!(validate_provider_request(&extra).is_err());
        let mut stale = plan.clone();
        stale.mutation_domains[0].expected_revision -= 1;
        assert!(
            validate_provider_response(&request, &ProviderResponse::Planned { plan: stale })
                .is_err()
        );
        let mut substituted = request.clone();
        substituted.control_results[0].output.resource.incarnation = "other".into();
        assert!(validate_provider_request(&substituted).is_err());
        let mut previous = request.clone();
        previous.control_results[0].output.resource.revision -= 1;
        previous.control_results[0].output.domain_observations[0].revision -= 1;
        assert!(validate_provider_request(&previous).is_ok());

        let mut capture = plan.clone();
        capture.candidate_constraint = Some(serde_json::from_value(json!({"capture_provider":"capture","selector_schema":"selector/1","resource_provider":"example.owner","resource_id":"resource","incarnation":"instance","execution_domain":"seat","mode":null,"plan_revision":1})).unwrap());
        assert!(
            validate_provider_response(
                &request,
                &ProviderResponse::Planned {
                    plan: capture.clone()
                }
            )
            .is_err()
        );
        capture
            .candidate_constraint
            .as_mut()
            .unwrap()
            .geometry_revision = Some(0);
        assert!(validate_resource_plan(&capture).is_err());
        capture
            .candidate_constraint
            .as_mut()
            .unwrap()
            .geometry_revision = Some(8);
        assert!(
            validate_provider_response(
                &request,
                &ProviderResponse::Planned {
                    plan: capture.clone()
                }
            )
            .is_ok()
        );
        let mut legacy = request.clone();
        legacy.version = CONTRACT_VERSION;
        legacy.operation = ProviderOperation::LifecycleCallback;
        legacy.dependency_inputs.clear();
        legacy.control_results.clear();
        legacy.admission_id = Some("admission".into());
        legacy.plan_revision = Some(1);
        legacy.approved_plan = Some(capture);
        let mut candidate: CaptureBinding = serde_json::from_value(json!({"capture_provider":"capture","selector_schema":"selector/1","selector":{},"resource":resource,"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":9,"generation":2})).unwrap();
        let prepared = |candidate| ProviderResponse::Prepared {
            candidate: Some(candidate),
            obligations: vec![],
            outputs: vec![],
        };
        assert!(validate_provider_response(&legacy, &prepared(candidate.clone())).is_err());
        candidate.geometry_revision = 8;
        assert!(validate_provider_response(&legacy, &prepared(candidate)).is_ok());
        let constraint = legacy
            .approved_plan
            .as_mut()
            .unwrap()
            .candidate_constraint
            .as_mut()
            .unwrap();
        constraint.geometry_revision = None;
        assert!(
            serde_json::to_value(constraint)
                .unwrap()
                .get("geometry_revision")
                .is_none()
        );
    }

    #[test]
    fn reserved_action_status_round_trips() {
        let status = ActionStatus::Reserved;
        assert_eq!(serde_json::to_value(&status).unwrap(), json!("reserved"));
        assert_eq!(
            serde_json::from_value::<ActionStatus>(json!("reserved")).unwrap(),
            status
        );
    }

    #[test]
    fn descriptor_rejects_foreign_actions_and_unplanned_mutations() {
        let method = MethodRef {
            service: "example.provider/1".into(),
            method: "execute".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        let mut descriptor = ExtensionDescriptor {
            provider: "example.provider".into(),
            revision: 1,
            options: vec![],
            actions: vec![ActionDescriptor {
                id: "example.provider/remove".into(),
                label: "Remove".into(),
                description: String::new(),
                execution: ExecutionClass::SessionMutation,
                arguments: Default::default(),
                allowed_targets: vec![TargetKind::Session],
                required_capabilities: vec![],
                plan: None,
                execute: method.clone(),
                status: None,
            }],
            capabilities: vec![],
            settings_validate: method.clone(),
        };
        assert!(validate_descriptor(&descriptor).is_err());
        descriptor.actions[0].plan = Some(method);
        assert!(validate_descriptor(&descriptor).is_ok());
        descriptor.actions[0].id = "other/remove".into();
        assert!(validate_descriptor(&descriptor).is_err());
    }

    #[test]
    fn plans_reject_cycles_and_callbacks_require_admission() {
        let method = MethodRef {
            service: "example.provider/1".into(),
            method: "cleanup".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        let obligation = |id: &str, dependency: &str| ResourceObligation {
            obligation_id: id.into(),
            provider: "example.provider".into(),
            resource_id: "resource".into(),
            mutation_domain: None,
            depends_on: vec![dependency.into()],
            cleanup_method: method.clone(),
            payload: json!({}),
        };
        let mut plan = ResourcePlan {
            plan_id: "plan".into(),
            revision: 1,
            resources: vec![],
            mutation_domains: vec![],
            required_capabilities: vec![],
            constraints: json!({}),
            obligations: vec![obligation("a", "b"), obligation("b", "a")],
            affected_consumers: vec![],
            consumer_change: ConsumerChange::Live,
            candidate_constraint: None,
            allowed_degradation: vec![],
            control_proposals: vec![],
        };
        assert!(validate_resource_plan(&plan).is_err());
        plan.obligations[1].depends_on.clear();
        assert!(validate_resource_plan(&plan).is_ok());
        let mut request = ProviderRequest {
            provider_sequence: 1,
            operation: ProviderOperation::LifecycleCallback,
            version: 1,
            transition_id: "transition".into(),
            operation_id: "operation".into(),
            target: LifecycleTarget::Validation {
                host_epoch: "epoch".into(),
                validation_id: "probe".into(),
                generation: 1,
            },
            phase: LifecyclePhase::StreamPrepare,
            settings_revision: "settings".into(),
            settings: Default::default(),
            intent: None,
            reason: None,
            requested_video: None,
            consumer_requirements: ConsumerRequirements {
                execution_domain: "desktop".into(),
                capture_providers: vec!["capture".into()],
                selector_schemas: vec!["selector/1".into()],
            },
            remaining_ms: 100,
            plan_revision: Some(1),
            admission_id: None,
            approved_plan: Some(plan),
            dependency_inputs: vec![],
            control_results: vec![],
        };
        assert!(validate_provider_request(&request).is_err());
        request.admission_id = Some("admitted".into());
        assert!(validate_provider_request(&request).is_ok());
        request.provider_sequence = 0;
        assert!(validate_provider_request(&request).is_err());
        request.provider_sequence = 1;
        let response = ProviderResponse::Cleanup {
            outcomes: vec![CleanupOutcome {
                obligation_id: "foreign".into(),
                state: CleanupState::Released,
                reason: None,
            }],
        };
        assert!(validate_provider_response(&request, &response).is_err());
        let plan = request.approved_plan.as_mut().unwrap();
        plan.resources.push(PlannedResource {
            provider: "display".into(),
            resource_id: "output".into(),
            incarnation: None,
            expected_revision: None,
            execution_domain: "other-domain".into(),
            mutation_domains: vec![],
            access: ResourceAccess::Exclusive,
        });
        plan.candidate_constraint = Some(BindingConstraint {
            capture_provider: "capture".into(),
            selector_schema: "selector/1".into(),
            resource_provider: "display".into(),
            resource_id: "output".into(),
            incarnation: None,
            execution_domain: "other-domain".into(),
            mode: None,
            plan_revision: 1,
            geometry_revision: None,
        });
        assert!(validate_provider_request(&request).is_err());
    }

    #[test]
    fn keybindings_reject_duplicate_modifiers_and_invalid_arguments() {
        let method = MethodRef {
            service: "example.provider/1".into(),
            method: "execute".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        let action = ActionDescriptor {
            id: "example.provider/do".into(),
            label: "Do".into(),
            description: String::new(),
            execution: ExecutionClass::ReadOnly,
            arguments: Default::default(),
            allowed_targets: vec![TargetKind::Session],
            required_capabilities: vec![],
            plan: None,
            execute: method,
            status: None,
        };
        let modifier = KeyIdentity::LinuxEvdev { code: 29 };
        let mut binding = Keybinding {
            id: "binding".into(),
            enabled: true,
            source: BindingSource::SessionInput,
            context: "active".into(),
            trigger: KeyIdentity::LinuxEvdev { code: 30 },
            modifiers: vec![modifier.clone()],
            propagation: BindingPropagation::Forward,
            action_id: action.id.clone(),
            arguments: Default::default(),
            target: BindingTarget::CurrentSession,
        };
        assert!(validate_keybinding(&binding, &action).is_ok());
        binding.modifiers.push(modifier);
        assert!(validate_keybinding(&binding, &action).is_err());
        binding.modifiers.clear();
        binding.arguments.insert("unexpected".into(), json!(true));
        assert!(validate_keybinding(&binding, &action).is_err());
    }

    #[test]
    fn targets_identify_kind_and_reject_unknown_authority() {
        let target: LifecycleTarget = serde_json::from_value(json!({"kind":"validation", "host_epoch":"epoch", "validation_id":"probe", "generation":1})).unwrap();
        assert!(matches!(target, LifecycleTarget::Validation { .. }));
        assert!(
            serde_json::from_value::<ActionTarget>(
                json!({"kind":"host", "host_epoch":"epoch", "administrator":true})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActionTarget>(
                json!({"kind":"session", "host_epoch":"epoch", "session_id":"s"})
            )
            .is_err()
        );
    }

    #[test]
    fn capture_binding_defaults_to_exact_selection() {
        let value = json!({"capture_provider":"capture", "selector_schema":"selector/1", "selector":{}, "resource":{"provider":"display", "resource_id":"resource", "incarnation":"instance", "revision":1, "execution_domain":"desktop", "mutation_domain":null}, "mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}, "plan_revision":3, "geometry_revision":2, "generation":1});
        let binding: CaptureBinding = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(binding.fallback, CaptureFallback::Forbidden);
        assert!(validate_capture_binding(&binding).is_ok());
        let mut invalid_binding = binding.clone();
        invalid_binding.resource.incarnation.clear();
        assert!(validate_capture_binding(&invalid_binding).is_err());
        let mut unsupported = value;
        unsupported["fallback"] = json!("approved");
        assert!(serde_json::from_value::<CaptureBinding>(unsupported).is_err());
    }
}
