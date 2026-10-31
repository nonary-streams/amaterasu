//! Coordinator-owned prepare admission. Persist state before publishing it.
//!
//! Initial observations from an authenticated selected provider are accepted
//! under installed host policy. Existing observations and locks are authoritative
//! coordinator state; provider plans cannot replace them. The provider must also
//! revalidate native state under the callback before performing an effect.
use std::collections::{BTreeMap, BTreeSet};

use amaterasu_extension_api::{
    CleanupOutcome, CleanupState, LifecycleTarget, MethodRef, ResourceAccess,
    ResourceControlRequest, ResourceOutput, ResourcePlan, SchemaRef,
};
use amaterasu_module_sdk::ModuleResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::{MethodSettings, ParticipantSettings};
use crate::plan_validation::{AdmissionContext, validate_effect_plan};
use crate::planning::{Access, DomainState, PlanningSnapshot, ResourceRef};
use crate::prepare_driver::PrepareAuthority;
use crate::provider_dispatch::ProviderCall;

const MAX_ADMISSIONS: usize = 128;
const MAX_PROVIDERS: usize = 128;

/// Construct only from verified installed descriptor/schema and broker query
/// results, never from the requested operation or provider's plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSelection {
    pub participant: ParticipantSettings,
    pub approvals: Vec<MethodApproval>,
    pub capabilities: BTreeSet<String>,
    pub cleanup_methods: Vec<MethodRef>,
    pub authorized_borrowers: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodApproval {
    /// Includes schema revision selected from the installed descriptor.
    pub method: MethodSettings,
    /// Exact broker query response; dispatch rechecks this before calling.
    pub broker_approval: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub provider: String,
    pub selection: ProviderSelection,
    pub plan: ResourcePlan,
    pub rights: Vec<(String, Access)>,
}

/// Installed policy, independently selected from the dependent's proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPolicy {
    pub dependent_provider: String,
    pub owner_provider: String,
    pub owner_service: String,
    pub owner_method: MethodRef,
    pub argument_schema: SchemaRef,
    pub output_capability: String,
    pub output_schema: SchemaRef,
    pub max_access: ResourceAccess,
    pub mutation_domains: BTreeSet<String>,
}

/// Constructed by the coordinator from the frozen installed graph and its
/// journaled, authenticated producer output. Never deserialize from a request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlContext {
    pub policy: ControlPolicy,
    pub output: ResourceOutput,
    pub target: LifecycleTarget,
    pub transition_id: String,
    /// Coordinator-read generation of the session containing a stream target.
    pub parent_generation: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPermit {
    pub context: ControlContext,
    pub request: ResourceControlRequest,
    pub consumed: bool,
    #[serde(default)]
    pub settled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlPermitUse {
    Execute,
    ReattachOnly,
}

/// The entire state must share one durable serialized authority/journal with
/// the prepare driver, or writes must use serialized atomic compare-and-set.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityState {
    pub sequences: BTreeMap<String, u64>,
    /// Legacy random IDs from existing journals, retained only for migration.
    #[serde(default)]
    pub issued_operation_ids: BTreeSet<String>,
    /// Durable namespace for both effect and poll broker operation IDs.
    #[serde(default)]
    pub operation_counter: u64,
    pub snapshot: PlanningSnapshot,
    pub admissions: BTreeMap<String, Admission>,
    pub domain_locks: BTreeMap<String, String>,
    /// First authenticated provider responsible for each domain observation.
    pub domain_owners: BTreeMap<String, String>,
    /// A completed callback may have changed this domain; a fresh authenticated
    /// provider plan must refresh it before another admission.
    pub refresh_required: BTreeSet<String>,
    /// Retains the full plan and cleanup obligations after lock release.
    pub settled: BTreeMap<String, Admission>,
    #[serde(default)]
    pub control_permits: BTreeMap<String, ControlPermit>,
}

pub trait AuthorityJournal {
    fn persist(&mut self, state: &AuthorityState) -> ModuleResult<()>;
}

pub struct CoordinatorPrepareAuthority<J> {
    state: AuthorityState,
    selections: BTreeMap<String, ProviderSelection>,
    journal: J,
    poisoned: bool,
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn access_level(access: &ResourceAccess) -> u8 {
    match access {
        ResourceAccess::Read => 0,
        ResourceAccess::Shared => 1,
        ResourceAccess::Exclusive => 2,
    }
}

fn valid_schema(schema: &SchemaRef) -> bool {
    valid_id(&schema.id)
        && schema.major != 0
        && schema.digest.len() == 64
        && schema
            .digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn output_in_scope(context: &ControlContext) -> bool {
    match (&context.output.origin, &context.target) {
        (
            LifecycleTarget::Session {
                host_epoch: a,
                session_id: b,
                generation: g,
            },
            LifecycleTarget::Stream {
                host_epoch: c,
                session_id: d,
                ..
            },
        ) => a == c && b == d && context.parent_generation == Some(*g),
        (origin, target) => origin == target,
    }
}

fn validate_control_binding(
    admission: &Admission,
    context: &ControlContext,
    request: &ResourceControlRequest,
    snapshot: &PlanningSnapshot,
) -> ModuleResult<()> {
    let policy = &context.policy;
    let output = &context.output;
    let proposal = &request.proposal;
    let resource = &proposal.resource;
    let domains: BTreeSet<_> = proposal.mutation_domains.iter().cloned().collect();
    if !valid_id(&policy.dependent_provider)
        || !valid_id(&policy.owner_provider)
        || !valid_id(&policy.owner_service)
        || !valid_id(&policy.output_capability)
        || !valid_schema(&policy.argument_schema)
        || !valid_schema(&policy.output_schema)
        || !valid_id(&policy.owner_method.method)
        || policy.owner_method.interface_major == 0
        || policy.owner_method.schema_revision == 0
        || !valid_id(&request.transition_id)
        || !valid_id(&request.operation_id)
        || !valid_id(&proposal.proposal_id)
        || !valid_id(&proposal.output_id)
        || request.provider_sequence == 0
        || request.version != amaterasu_extension_api::COMPOSITION_VERSION
        || request.remaining_ms == 0
        || request.target != context.target
        || proposal.target != context.target
        || request.transition_id != context.transition_id
        || admission.provider != policy.dependent_provider
        || !admission
            .selection
            .participant
            .depends_on
            .contains(&policy.owner_provider)
        || proposal.owner_provider != policy.owner_provider
        || proposal.owner_service != policy.owner_service
        || proposal.method != policy.owner_method
        || proposal.method.service != policy.owner_service
        || proposal.argument_schema != policy.argument_schema
        || !proposal.arguments.is_object()
        || output.output_id != proposal.output_id
        || !output_in_scope(context)
        || output.producer != policy.owner_provider
        || output.producer_service != policy.owner_service
        || output.capability != policy.output_capability
        || output.schema != policy.output_schema
        || output.resource != *resource
        || resource.provider != policy.owner_provider
        || domains.len() != proposal.mutation_domains.len()
        || domains.is_empty()
        || domains != resource.mutation_domains.iter().cloned().collect()
        || !domains.is_subset(&policy.mutation_domains)
        || access_level(&proposal.access) > access_level(&policy.max_access)
        || admission
            .plan
            .control_proposals
            .iter()
            .filter(|entry| entry.proposal_id == proposal.proposal_id)
            .count()
            != 1
        || !admission.plan.control_proposals.contains(proposal)
    {
        return Err("control proposal differs from frozen policy or output".into());
    }
    let planned = admission
        .plan
        .resources
        .iter()
        .find(|planned| {
            planned.provider == resource.provider && planned.resource_id == resource.resource_id
        })
        .ok_or("control resource was not admitted")?;
    if planned.incarnation.as_deref() != Some(&resource.incarnation)
        || planned.expected_revision != Some(resource.revision)
        || planned.execution_domain != resource.execution_domain
        || planned
            .mutation_domains
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            != domains
        || access_level(&planned.access) < access_level(&proposal.access)
        || domains.iter().any(|domain| {
            admission
                .rights
                .iter()
                .find(|(name, _)| name == domain)
                .is_none_or(|(_, right)| {
                    let level = match right {
                        Access::Read => 0,
                        Access::Shared => 1,
                        Access::Exclusive => 2,
                    };
                    level < access_level(&proposal.access)
                })
        })
        || snapshot
            .resources
            .get(&resource.provider)
            .and_then(|items| items.get(&resource.resource_id))
            != Some(&ResourceRef {
                provider: resource.provider.clone(),
                resource_id: resource.resource_id.clone(),
                incarnation: resource.incarnation.clone(),
                revision: resource.revision,
                execution_domain: resource.execution_domain.clone(),
                mutation_domains: domains,
            })
    {
        return Err("control resource or rights are stale".into());
    }
    Ok(())
}

fn validate_selection(selection: &ProviderSelection) -> ModuleResult<()> {
    let participant = &selection.participant;
    let mut methods = vec![
        &participant.plan_method,
        &participant.callback_method,
        &participant.status_method,
    ];
    if let Some(method) = &participant.binding_validate_method {
        methods.push(method);
    }
    if !valid_id(&participant.provider) || selection.approvals.len() != methods.len() {
        return Err("invalid installed selection".into());
    }
    for method in methods {
        if method.schema_revision == 0
            || method.interface_major == 0
            || !valid_id(&method.service)
            || !valid_id(&method.method)
        {
            return Err("invalid installed method".into());
        }
        let matches: Vec<_> = selection
            .approvals
            .iter()
            .filter(|entry| &entry.method == method)
            .collect();
        if matches.len() != 1 {
            return Err("missing or duplicate method approval".into());
        }
        let approval = &matches[0].broker_approval;
        let approved_method = &approval["method"];
        if approval["provider"].as_str() != Some(&participant.provider)
            || approved_method["service"].as_str() != Some(&method.service)
            || approved_method["method"].as_str() != Some(&method.method)
            || approved_method["interface_major"].as_u64()
                != Some(u64::from(method.interface_major))
            || approved_method["interface_minor"].as_u64()
                != Some(u64::from(method.interface_minor))
        {
            return Err("broker approval differs from installed method".into());
        }
    }
    Ok(())
}

fn fresh_operation(state: &mut AuthorityState) -> ModuleResult<String> {
    loop {
        state.operation_counter = state
            .operation_counter
            .checked_add(1)
            .ok_or("operation ID space exhausted")?;
        let id = format!("lifecycle-{}", state.operation_counter);
        if !state.issued_operation_ids.contains(&id) {
            return Ok(id);
        }
    }
}

fn validate_domain_advance(old: &DomainState, current: &DomainState) -> ModuleResult<()> {
    if current.revision < old.revision
        || (current != old && current.revision == old.revision)
        || old.borrowers.iter().any(|(id, revision)| {
            current
                .borrowers
                .get(id)
                .is_some_and(|current| current != revision)
        })
        || current.borrowers.keys().any(|id| !valid_id(id))
    {
        return Err("stale or incomplete borrower observation".into());
    }
    Ok(())
}

/// Reap only orphaned resources whose complete domain observations prove they
/// are idle. Stale or borrowed domains conservatively retain their resources.
fn prune_idle_orphan_resources(state: &mut AuthorityState) {
    let referenced: BTreeSet<(String, String)> = state
        .admissions
        .values()
        .chain(state.settled.values())
        .flat_map(|admission| {
            admission
                .plan
                .resources
                .iter()
                .map(|resource| (resource.provider.clone(), resource.resource_id.clone()))
        })
        .collect();
    for (provider, resources) in &mut state.snapshot.resources {
        resources.retain(|id, resource| {
            referenced.contains(&(provider.clone(), id.clone()))
                || resource.mutation_domains.is_empty()
                || resource.mutation_domains.iter().any(|domain| {
                    state.domain_locks.contains_key(domain)
                        || state.refresh_required.contains(domain)
                        || state
                            .snapshot
                            .domains
                            .get(domain)
                            .is_none_or(|observation| !observation.borrowers.is_empty())
                })
        });
    }
    state
        .snapshot
        .resources
        .retain(|_, resources| !resources.is_empty());
}

fn validate_materialized_resource(
    admission: &Admission,
    resource: &ResourceRef,
) -> ModuleResult<()> {
    if resource.provider != admission.provider
        || !valid_id(&resource.resource_id)
        || !valid_id(&resource.incarnation)
        || !valid_id(&resource.execution_domain)
        || resource.revision == 0
    {
        return Err("invalid materialized resource".into());
    }
    let mut matches = admission.plan.resources.iter().filter(|planned| {
        planned.provider == resource.provider && planned.resource_id == resource.resource_id
    });
    let planned = matches.next().ok_or("resource was not admitted")?;
    if matches.next().is_some()
        || planned.execution_domain != resource.execution_domain
        || planned
            .mutation_domains
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            != resource.mutation_domains
        || planned
            .incarnation
            .as_ref()
            .is_some_and(|incarnation| incarnation != &resource.incarnation)
        || planned.expected_revision.is_some_and(|revision| {
            if planned.access == amaterasu_extension_api::ResourceAccess::Read {
                resource.revision != revision
            } else {
                resource.revision < revision
            }
        })
        || resource
            .mutation_domains
            .iter()
            .any(|domain| !admission.rights.iter().any(|(owned, _)| owned == domain))
    {
        return Err("materialized resource exceeds admission".into());
    }
    Ok(())
}

impl<J: AuthorityJournal> CoordinatorPrepareAuthority<J> {
    /// `selections` and all recovered coordinator state must originate from
    /// verified installed metadata and the private durable journal respectively.
    pub fn new(
        state: AuthorityState,
        selections: Vec<ProviderSelection>,
        journal: J,
    ) -> ModuleResult<Self> {
        if selections.len() > MAX_PROVIDERS
            || state.sequences.len() > MAX_PROVIDERS
            || state.admissions.len() + state.settled.len() > MAX_ADMISSIONS
            || state.issued_operation_ids.len() > 4096
            || state.control_permits.len() > MAX_ADMISSIONS
            || state.issued_operation_ids.iter().any(|id| !valid_id(id))
        {
            return Err("authority capacity exceeded".into());
        }
        let mut selected = BTreeMap::new();
        for selection in selections {
            validate_selection(&selection)?;
            if selected
                .insert(selection.participant.provider.clone(), selection)
                .is_some()
            {
                return Err("duplicate provider selection".into());
            }
        }
        for (id, admission) in &state.admissions {
            if !valid_id(id) || admission.provider != admission.selection.participant.provider {
                return Err("invalid recovered admission".into());
            }
            validate_selection(&admission.selection)?;
            let context = AdmissionContext {
                snapshot: &state.snapshot,
                complete: true,
                approved_capabilities: &admission.selection.capabilities,
                approved_cleanup_methods: &admission.selection.cleanup_methods,
                authorized_borrowers: &admission.selection.authorized_borrowers,
            };
            if validate_effect_plan(&admission.plan, &context)? != admission.rights
                || admission
                    .rights
                    .iter()
                    .any(|(domain, _)| state.domain_locks.get(domain) != Some(id))
            {
                return Err("invalid recovered admission rights".into());
            }
        }
        for (id, admission) in &state.settled {
            if !valid_id(id)
                || state.admissions.contains_key(id)
                || admission.provider != admission.selection.participant.provider
            {
                return Err("invalid settled admission".into());
            }
            validate_selection(&admission.selection)?;
            amaterasu_extension_api::validate_resource_plan(&admission.plan)?;
        }
        let mut control_operations = BTreeSet::new();
        let mut control_sequences = BTreeSet::new();
        for (id, permit) in &state.control_permits {
            let admission = state
                .admissions
                .get(&permit.request.admission_id)
                .ok_or("orphaned control permit")?;
            if !valid_id(id)
                || permit.request.permit_id != *id
                || (permit.settled && !permit.consumed)
                || !control_operations.insert(&permit.request.operation_id)
                || !control_sequences.insert((
                    &permit.context.policy.owner_provider,
                    permit.request.provider_sequence,
                ))
                || state
                    .sequences
                    .get(&permit.context.policy.owner_provider)
                    .is_none_or(|current| *current < permit.request.provider_sequence)
            {
                return Err("invalid recovered control permit".into());
            }
            validate_control_binding(admission, &permit.context, &permit.request, &state.snapshot)?;
        }
        if state.refresh_required.iter().any(|domain| {
            !state.domain_owners.contains_key(domain) || state.domain_locks.contains_key(domain)
        }) {
            return Err("invalid domain refresh state".into());
        }
        if state.domain_owners.len() != state.snapshot.domains.len()
            || state.domain_owners.iter().any(|(domain, provider)| {
                !state.snapshot.domains.contains_key(domain) || !valid_id(provider)
            })
        {
            return Err("invalid domain observation ownership".into());
        }
        if state.domain_locks.iter().any(|(domain, owner)| {
            state
                .admissions
                .get(owner)
                .is_none_or(|admission| !admission.rights.iter().any(|(name, _)| name == domain))
        }) || state
            .sequences
            .iter()
            .any(|(provider, sequence)| !valid_id(provider) || *sequence == 0)
        {
            return Err("invalid recovered authority state".into());
        }
        Ok(Self {
            state,
            selections: selected,
            journal,
            poisoned: false,
        })
    }

    pub fn state(&self) -> &AuthorityState {
        &self.state
    }

    /// The coordinator calls this only after an authenticated, validated terminal
    /// callback. Cleanup obligations remain outstanding in settled history.
    /// Unknown callback outcomes must retain the admission and its locks.
    pub fn finish_admission(&mut self, provider: &str, admission_id: &str) -> ModuleResult<()> {
        self.finish_admission_with_resource(provider, admission_id, None)
    }

    /// Materialize only the exact resource reserved by the admitted plan and
    /// persist it together with the terminal admission transition.
    pub fn finish_admission_with_resource(
        &mut self,
        provider: &str,
        admission_id: &str,
        resource: Option<ResourceRef>,
    ) -> ModuleResult<()> {
        if self.poisoned || !self.selections.contains_key(provider) {
            return Err("provider is not approved".into());
        }
        if self
            .state
            .control_permits
            .values()
            .any(|permit| permit.request.admission_id == admission_id && !permit.settled)
        {
            return Err("control outcome remains unsettled".into());
        }
        if let Some(settled) = self.state.settled.get(admission_id) {
            if settled.provider != provider {
                return Err("admission provider mismatch".into());
            }
            if let Some(resource) = resource {
                validate_materialized_resource(settled, &resource)?;
                if self
                    .state
                    .snapshot
                    .resources
                    .get(provider)
                    .and_then(|items| items.get(&resource.resource_id))
                    != Some(&resource)
                {
                    return Err("settled resource mismatch".into());
                }
            }
            return Ok(());
        }
        let admission = self
            .state
            .admissions
            .get(admission_id)
            .ok_or("unknown admission")?
            .clone();
        if admission.provider != provider
            || admission.rights.iter().any(|(domain, _)| {
                self.state.domain_locks.get(domain).map(String::as_str) != Some(admission_id)
            })
        {
            return Err("admission provider or rights mismatch".into());
        }
        let mut next = self.state.clone();
        if let Some(resource) = resource {
            validate_materialized_resource(&admission, &resource)?;
            if let Some(old) = next
                .snapshot
                .resources
                .get(provider)
                .and_then(|resources| resources.get(&resource.resource_id))
                && (old.execution_domain != resource.execution_domain
                    || old.mutation_domains != resource.mutation_domains
                    || resource.revision < old.revision
                    || (resource != *old && resource.revision == old.revision))
            {
                return Err("materialized resource conflicts with prior observation".into());
            }
            next.snapshot
                .resources
                .entry(provider.to_owned())
                .or_default()
                .insert(resource.resource_id.clone(), resource);
        }
        for (domain, _) in &admission.rights {
            next.domain_locks.remove(domain);
            next.refresh_required.insert(domain.clone());
        }
        next.admissions.remove(admission_id);
        next.control_permits
            .retain(|_, permit| permit.request.admission_id != admission_id);
        if !admission.plan.obligations.is_empty() {
            next.settled.insert(admission_id.to_owned(), admission);
        }
        self.persist(next)
    }

    /// Retire a retained plan only after authenticated cleanup confirms every
    /// obligation released. Deferred or uncertain restoration must stay retained.
    /// The caller must bind these outcomes to the original admission and provider.
    pub fn retire_settled(
        &mut self,
        provider: &str,
        admission_id: &str,
        outcomes: &[CleanupOutcome],
    ) -> ModuleResult<()> {
        if self.poisoned || !self.selections.contains_key(provider) {
            return Err("provider is not approved".into());
        }
        let admission = self
            .state
            .settled
            .get(admission_id)
            .ok_or("unknown settled admission")?;
        if admission.provider != provider || outcomes.len() != admission.plan.obligations.len() {
            return Err("cleanup does not match settled admission".into());
        }
        let mut seen = BTreeSet::new();
        if outcomes.iter().any(|outcome| {
            outcome.state != CleanupState::Released
                || !seen.insert(&outcome.obligation_id)
                || !admission
                    .plan
                    .obligations
                    .iter()
                    .any(|obligation| obligation.obligation_id == outcome.obligation_id)
        }) {
            return Err("cleanup is incomplete or unconfirmed".into());
        }
        let mut next = self.state.clone();
        next.settled.remove(admission_id);
        prune_idle_orphan_resources(&mut next);
        self.persist(next)
    }

    /// Refresh an already observed domain only when it is unlocked and the
    /// caller has authenticated its original provider's complete readback.
    /// The authenticated provider supplies a complete native readback. A newer
    /// revision may remove retired native borrowers; locks still fence refresh.
    pub fn refresh_domain(
        &mut self,
        provider: &str,
        domain: &str,
        observation: DomainState,
    ) -> ModuleResult<()> {
        if self.poisoned
            || !self.selections.contains_key(provider)
            || self.state.domain_owners.get(domain).map(String::as_str) != Some(provider)
            || self.state.domain_locks.contains_key(domain)
        {
            return Err("domain refresh not authorized or locked".into());
        }
        let old = self
            .state
            .snapshot
            .domains
            .get(domain)
            .ok_or("unknown domain")?;
        validate_domain_advance(old, &observation)?;
        let mut next = self.state.clone();
        next.snapshot.domains.insert(domain.to_owned(), observation);
        self.persist(next)
    }

    /// Refresh an already known resource from its authenticated owner. Domain
    /// changes need a coordinated plan and are not accepted by this method.
    pub fn refresh_resource(
        &mut self,
        provider: &str,
        observation: ResourceRef,
    ) -> ModuleResult<()> {
        if self.poisoned
            || !self.selections.contains_key(provider)
            || observation.provider != provider
            || !valid_id(&observation.resource_id)
            || !valid_id(&observation.incarnation)
            || !valid_id(&observation.execution_domain)
            || observation.revision == 0
        {
            return Err("invalid resource refresh".into());
        }
        let old = self
            .state
            .snapshot
            .resources
            .get(provider)
            .and_then(|resources| resources.get(&observation.resource_id))
            .ok_or("unknown resource")?;
        if old.mutation_domains != observation.mutation_domains
            || observation.revision < old.revision
            || (observation != *old && observation.revision == old.revision)
            || observation.mutation_domains.iter().any(|domain| {
                self.state.domain_locks.contains_key(domain)
                    || self.state.domain_owners.get(domain).map(String::as_str) != Some(provider)
            })
        {
            return Err("resource refresh is stale, unowned or locked".into());
        }
        let mut next = self.state.clone();
        next.snapshot
            .resources
            .get_mut(provider)
            .expect("checked owner")
            .insert(observation.resource_id.clone(), observation);
        self.persist(next)
    }

    /// Publish one complete native readback without exposing a partially
    /// refreshed snapshot. The selected owner supplies the observations; this
    /// method does not independently inspect the operating system. No locked
    /// domain, new resource, or changed execution domain can enter through it.
    pub fn refresh_observations(
        &mut self,
        provider: &str,
        domains: &[(String, DomainState)],
        resources: &[ResourceRef],
    ) -> ModuleResult<()> {
        if self.poisoned
            || !self.selections.contains_key(provider)
            || (domains.is_empty() && resources.is_empty())
            || domains.len() > MAX_ADMISSIONS
            || resources.len() > MAX_ADMISSIONS
        {
            return Err("invalid provider observation batch".into());
        }
        let mut next = self.state.clone();
        let mut seen_domains = BTreeSet::new();
        for (domain, observation) in domains {
            if !valid_id(domain)
                || !seen_domains.insert(domain)
                || observation.revision == 0
                || observation
                    .borrowers
                    .values()
                    .any(|generation| *generation == 0)
                || self.state.domain_owners.get(domain).map(String::as_str) != Some(provider)
                || self.state.domain_locks.contains_key(domain)
            {
                return Err("domain refresh not authorized or locked".into());
            }
            let old = self
                .state
                .snapshot
                .domains
                .get(domain)
                .ok_or("unknown domain")?;
            validate_domain_advance(old, observation)?;
            next.snapshot
                .domains
                .insert(domain.clone(), observation.clone());
            next.refresh_required.remove(domain);
        }
        let mut seen_resources = BTreeSet::new();
        for observation in resources {
            if observation.provider != provider
                || !valid_id(&observation.resource_id)
                || !valid_id(&observation.incarnation)
                || !valid_id(&observation.execution_domain)
                || observation.revision == 0
                || !seen_resources.insert(&observation.resource_id)
                || observation.mutation_domains.is_empty()
                || observation.mutation_domains.iter().any(|domain| {
                    self.state.domain_locks.contains_key(domain)
                        || self.state.domain_owners.get(domain).map(String::as_str)
                            != Some(provider)
                })
            {
                return Err("invalid resource observation".into());
            }
            let old = self
                .state
                .snapshot
                .resources
                .get(provider)
                .and_then(|resources| resources.get(&observation.resource_id));
            if let Some(old) = old {
                if old.mutation_domains != observation.mutation_domains
                    || old.execution_domain != observation.execution_domain
                    || observation.revision < old.revision
                    || (observation != old && observation.revision == old.revision)
                {
                    return Err("stale or substituted resource observation".into());
                }
            } else {
                let reserved = self.state.settled.values().any(|admission| {
                    admission.provider == provider
                        && admission.plan.resources.iter().any(|planned| {
                            planned.provider == provider
                                && planned.resource_id == observation.resource_id
                                && planned.incarnation.is_none()
                                && planned.expected_revision.is_none()
                                && planned.execution_domain == observation.execution_domain
                                && planned
                                    .mutation_domains
                                    .iter()
                                    .cloned()
                                    .collect::<BTreeSet<_>>()
                                    == observation.mutation_domains
                        })
                });
                if !reserved {
                    return Err("unknown resource".into());
                }
            }
            next.snapshot
                .resources
                .entry(provider.to_owned())
                .or_default()
                .insert(observation.resource_id.clone(), observation.clone());
        }
        prune_idle_orphan_resources(&mut next);
        self.persist(next)
    }

    /// Issue only from a locked coordinator journal lookup of the canonical
    /// dependency output and frozen installed policy. `validate_arguments`
    /// must resolve the digest-pinned schema and validate the actual arguments.
    pub fn issue_control_permit(
        &mut self,
        participant: &ParticipantSettings,
        context: &ControlContext,
        request: &mut ResourceControlRequest,
        validate_arguments: impl Fn(&SchemaRef, &Value) -> ModuleResult<()>,
    ) -> ModuleResult<()> {
        if !request.permit_id.is_empty() || self.poisoned {
            return Err("invalid permit issuance".into());
        }
        if let Some((id, existing)) = self
            .state
            .control_permits
            .iter()
            .find(|(_, permit)| permit.request.operation_id == request.operation_id)
        {
            let mut proposed = request.clone();
            proposed.permit_id = id.clone();
            if existing.context != *context || existing.request != proposed {
                return Err("control operation conflicts with existing intent".into());
            }
            request.permit_id = id.clone();
            return Ok(());
        }
        let admission = self
            .state
            .admissions
            .get(&request.admission_id)
            .ok_or("unknown admission")?
            .clone();
        self.validate_rights(participant, &admission.plan, &request.admission_id)?;
        validate_control_binding(&admission, context, request, &self.state.snapshot)?;
        validate_arguments(&context.policy.argument_schema, &request.proposal.arguments)?;
        if self.state.sequences.get(&context.policy.owner_provider)
            != Some(&request.provider_sequence)
            || self.state.control_permits.len() >= MAX_ADMISSIONS
            || self.state.control_permits.values().any(|permit| {
                permit.request.operation_id == request.operation_id
                    || (permit.context.policy.owner_provider == context.policy.owner_provider
                        && permit.request.provider_sequence == request.provider_sequence)
            })
        {
            return Err("control operation is not fresh".into());
        }
        let permit_id =
            amaterasu_storage::random_identifier().map_err(|_| "entropy unavailable")?;
        if self.state.control_permits.contains_key(&permit_id) {
            return Err("permit ID collision".into());
        }
        let mut frozen = request.clone();
        frozen.permit_id = permit_id.clone();
        let mut next = self.state.clone();
        next.control_permits.insert(
            permit_id.clone(),
            ControlPermit {
                context: context.clone(),
                request: frozen,
                consumed: false,
                settled: false,
            },
        );
        self.persist(next)?;
        request.permit_id = permit_id;
        Ok(())
    }

    /// Authenticated owner consumes exactly once. Replays may only reattach to
    /// status; they never authorize another native effect.
    pub fn consume_control_permit(
        &mut self,
        owner_caller: &str,
        participant: &ParticipantSettings,
        context: &ControlContext,
        request: &ResourceControlRequest,
        validate_arguments: impl Fn(&SchemaRef, &Value) -> ModuleResult<()>,
    ) -> ModuleResult<ControlPermitUse> {
        if self.poisoned {
            return Err("authority journal requires recovery".into());
        }
        let permit = self
            .state
            .control_permits
            .get(&request.permit_id)
            .ok_or("unknown control permit")?;
        if owner_caller != context.policy.owner_provider
            || permit.context != *context
            || permit.request != *request
        {
            return Err("control permit binding mismatch".into());
        }
        if permit.consumed {
            return Ok(ControlPermitUse::ReattachOnly);
        }
        let admission = self
            .state
            .admissions
            .get(&request.admission_id)
            .ok_or("unknown admission")?
            .clone();
        self.validate_rights(participant, &admission.plan, &request.admission_id)?;
        validate_control_binding(&admission, context, request, &self.state.snapshot)?;
        validate_arguments(&context.policy.argument_schema, &request.proposal.arguments)?;
        let mut next = self.state.clone();
        next.control_permits
            .get_mut(&request.permit_id)
            .ok_or("unknown control permit")?
            .consumed = true;
        self.persist(next)?;
        Ok(ControlPermitUse::Execute)
    }

    /// The coordinator calls this after authenticating and validating a
    /// terminal owner result; unknown outcomes remain fenced indefinitely.
    pub fn settle_control_permit(&mut self, request: &ResourceControlRequest) -> ModuleResult<()> {
        let permit = self
            .state
            .control_permits
            .get(&request.permit_id)
            .ok_or("unknown control permit")?;
        if permit.request != *request || !permit.consumed {
            return Err("control permit not consumed".into());
        }
        if permit.settled {
            return Ok(());
        }
        let mut next = self.state.clone();
        next.control_permits
            .get_mut(&request.permit_id)
            .ok_or("unknown control permit")?
            .settled = true;
        self.persist(next)
    }

    fn persist(&mut self, next: AuthorityState) -> ModuleResult<()> {
        if self.poisoned {
            return Err("authority journal requires recovery".into());
        }
        if let Err(error) = self.journal.persist(&next) {
            self.poisoned = true;
            return Err(error);
        }
        self.state = next;
        Ok(())
    }

    fn selected(&self, participant: &ParticipantSettings) -> ModuleResult<&ProviderSelection> {
        let selection = self
            .selections
            .get(&participant.provider)
            .ok_or("provider approval unavailable")?;
        if &selection.participant != participant {
            return Err("installed participant changed".into());
        }
        Ok(selection)
    }
}

impl<J: AuthorityJournal> PrepareAuthority for CoordinatorPrepareAuthority<J> {
    fn allocate(&mut self, provider: &str) -> ModuleResult<(u64, String)> {
        if !self.selections.contains_key(provider) {
            return Err("provider approval unavailable".into());
        }
        let mut next = self.state.clone();
        let sequence = next
            .sequences
            .get(provider)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("provider sequence exhausted")?;
        if !next.sequences.contains_key(provider) && next.sequences.len() >= MAX_PROVIDERS {
            return Err("provider capacity exhausted".into());
        }
        let operation = fresh_operation(&mut next)?;
        next.sequences.insert(provider.to_owned(), sequence);
        self.persist(next)?;
        Ok((sequence, operation))
    }

    fn allocate_poll_id(&mut self, provider: &str) -> ModuleResult<String> {
        if !self.selections.contains_key(provider) {
            return Err("provider approval unavailable".into());
        }
        let mut next = self.state.clone();
        let operation = fresh_operation(&mut next)?;
        self.persist(next)?;
        Ok(operation)
    }

    fn approved_call(
        &mut self,
        participant: &ParticipantSettings,
        method: &MethodSettings,
        arguments: Value,
        operation_id: String,
    ) -> ModuleResult<ProviderCall> {
        if self.poisoned || !valid_id(&operation_id) || !arguments.is_object() {
            return Err("invalid approved call".into());
        }
        let selection = self.selected(participant)?;
        let entry = selection
            .approvals
            .iter()
            .find(|entry| &entry.method == method)
            .ok_or("method not approved")?;
        Ok(ProviderCall {
            service: method.service.clone(),
            method: method.method.clone(),
            interface_major: method.interface_major,
            interface_minor: method.interface_minor,
            approval: entry.broker_approval.clone(),
            arguments,
            timeout_ms: 5000,
            operation_id,
        })
    }

    fn admit_plan(
        &mut self,
        participant: &ParticipantSettings,
        plan: &ResourcePlan,
    ) -> ModuleResult<String> {
        if self.poisoned {
            return Err("authority journal requires recovery".into());
        }
        let selection = self.selected(participant)?.clone();
        amaterasu_extension_api::validate_resource_plan(plan)?;
        if self.state.admissions.len() + self.state.settled.len() >= MAX_ADMISSIONS {
            return Err("admission capacity exhausted".into());
        }
        let mut next = self.state.clone();
        let refreshable = next.refresh_required.clone();
        for domain in &plan.mutation_domains {
            if next.domain_locks.contains_key(&domain.domain) {
                return Err("mutation domain is reserved".into());
            }
            let observed = DomainState {
                revision: domain.expected_revision,
                borrowers: domain.borrowers.clone(),
            };
            if let Some(existing) = next.snapshot.domains.get(&domain.domain) {
                if refreshable.contains(&domain.domain) {
                    if next.domain_owners.get(&domain.domain) != Some(&participant.provider) {
                        return Err("only domain owner can refresh observation".into());
                    }
                    validate_domain_advance(existing, &observed)?;
                    next.snapshot
                        .domains
                        .insert(domain.domain.clone(), observed.clone());
                    next.refresh_required.remove(&domain.domain);
                } else if existing != &observed {
                    return Err("stale mutation domain observation".into());
                }
            }
            if !next.snapshot.domains.contains_key(&domain.domain) {
                next.domain_owners
                    .insert(domain.domain.clone(), participant.provider.clone());
            }
            next.snapshot
                .domains
                .entry(domain.domain.clone())
                .or_insert(observed);
        }
        for resource in &plan.resources {
            if resource.mutation_domains.is_empty()
                && resource.access != amaterasu_extension_api::ResourceAccess::Read
            {
                return Err("mutation has no shared domain".into());
            }
            match (&resource.incarnation, resource.expected_revision) {
                (Some(incarnation), Some(revision)) => {
                    let observed = ResourceRef {
                        provider: resource.provider.clone(),
                        resource_id: resource.resource_id.clone(),
                        incarnation: incarnation.clone(),
                        revision,
                        execution_domain: resource.execution_domain.clone(),
                        mutation_domains: resource.mutation_domains.iter().cloned().collect(),
                    };
                    let existing = next
                        .snapshot
                        .resources
                        .get(&resource.provider)
                        .and_then(|resources| resources.get(&resource.resource_id));
                    if existing.is_none() && resource.provider != participant.provider {
                        return Err("foreign resource observation unavailable".into());
                    }
                    if let Some(current) = existing
                        && current != &observed
                        && (resource.provider != participant.provider
                            || current.mutation_domains != observed.mutation_domains
                            || current.mutation_domains.is_empty()
                            || current.mutation_domains.iter().any(|domain| {
                                !refreshable.contains(domain)
                                    || next.domain_owners.get(domain) != Some(&participant.provider)
                            })
                            || observed.revision <= current.revision)
                    {
                        return Err("stale resource observation".into());
                    }
                    next.snapshot
                        .resources
                        .entry(resource.provider.clone())
                        .or_default()
                        .insert(resource.resource_id.clone(), observed);
                }
                (None, None) => {}
                _ => return Err("partially resolved resource".into()),
            }
        }
        let context = AdmissionContext {
            snapshot: &next.snapshot,
            complete: true,
            approved_capabilities: &selection.capabilities,
            approved_cleanup_methods: &selection.cleanup_methods,
            authorized_borrowers: &selection.authorized_borrowers,
        };
        let rights = validate_effect_plan(plan, &context)?;
        let id = amaterasu_storage::random_identifier().map_err(|_| "entropy unavailable")?;
        if next.admissions.contains_key(&id) || next.settled.contains_key(&id) {
            return Err("admission ID collision".into());
        }
        for (domain, _) in &rights {
            next.domain_locks.insert(domain.clone(), id.clone());
        }
        next.admissions.insert(
            id.clone(),
            Admission {
                provider: participant.provider.clone(),
                selection,
                plan: plan.clone(),
                rights,
            },
        );
        self.persist(next)?;
        Ok(id)
    }

    fn validate_rights(
        &mut self,
        participant: &ParticipantSettings,
        plan: &ResourcePlan,
        admission_id: &str,
    ) -> ModuleResult<()> {
        if self.poisoned {
            return Err("authority journal requires recovery".into());
        }
        let selection = self.selected(participant)?;
        let admission = self
            .state
            .admissions
            .get(admission_id)
            .ok_or("unknown admission")?;
        if &admission.selection != selection
            || &admission.plan != plan
            || admission.provider != participant.provider
            || admission.rights.iter().any(|(domain, _)| {
                self.state.domain_locks.get(domain).map(String::as_str) != Some(admission_id)
            })
        {
            return Err("admission changed or rights lost".into());
        }
        let context = AdmissionContext {
            snapshot: &self.state.snapshot,
            complete: true,
            approved_capabilities: &selection.capabilities,
            approved_cleanup_methods: &selection.cleanup_methods,
            authorized_borrowers: &selection.authorized_borrowers,
        };
        if validate_effect_plan(plan, &context)? != admission.rights {
            return Err("admission rights changed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct Journal {
        snapshots: Vec<AuthorityState>,
        fail: bool,
    }
    impl AuthorityJournal for Journal {
        fn persist(&mut self, state: &AuthorityState) -> ModuleResult<()> {
            if self.fail {
                return Err("write failed".into());
            }
            self.snapshots.push(state.clone());
            Ok(())
        }
    }
    fn method(name: &str) -> MethodSettings {
        MethodSettings {
            service: "display/1".into(),
            method: name.into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 2,
        }
    }
    fn selection() -> ProviderSelection {
        let participant = ParticipantSettings {
            provider: "display".into(),
            depends_on: vec![],
            plan_method: method("plan"),
            callback_method: method("callback"),
            status_method: method("status"),
            binding_validate_method: None,
            capabilities: ["display/change".into()].into(),
            cleanup_methods: vec![MethodRef {
                service: "display/1".into(),
                method: "cleanup".into(),
                interface_major: 1,
                interface_minor: 0,
                schema_revision: 2,
            }],
            settings_revision: "revision".into(),
            settings: json!({}),
        };
        let approvals = [&participant.plan_method, &participant.callback_method, &participant.status_method].iter().map(|method| MethodApproval { method:(*method).clone(), broker_approval:json!({"provider":"display", "method":{"service":method.service,"method":method.method,"interface_major":method.interface_major,"interface_minor":method.interface_minor}}) }).collect();
        ProviderSelection {
            participant,
            approvals,
            capabilities: ["display/change".into()].into(),
            cleanup_methods: vec![MethodRef {
                service: "display/1".into(),
                method: "cleanup".into(),
                interface_major: 1,
                interface_minor: 0,
                schema_revision: 2,
            }],
            authorized_borrowers: ["stream".into()].into(),
        }
    }
    fn plan() -> ResourcePlan {
        serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[{"provider":"display","resource_id":"output","incarnation":"native","expected_revision":2,"execution_domain":"seat","mutation_domains":["topology"],"access":"exclusive"}],"mutation_domains":[{"domain":"topology","expected_revision":4,"borrowers":{"stream":1},"access":"exclusive"}],"required_capabilities":["display/change"],"constraints":{},"obligations":[{"obligation_id":"restore","provider":"display","resource_id":"output","mutation_domain":"topology","depends_on":[],"cleanup_method":{"service":"display/1","method":"cleanup","interface_major":1,"interface_minor":0,"schema_revision":2},"payload":{}}],"affected_consumers":[],"consumer_change":"restart_pipeline","candidate_constraint":null,"allowed_degradation":[]})).unwrap()
    }

    #[test]
    fn control_permit_fences_forgery_replay_and_uncertain_effect() {
        let mut selected = selection();
        selected.participant.depends_on.push("owner".into());
        let mut plan = plan();
        plan.resources[0].provider = "owner".into();
        plan.obligations[0].provider = "owner".into();
        let target =
            json!({"kind":"session", "host_epoch":"host", "session_id":"session", "generation":1});
        let schema = json!({"id":"schema", "major":1, "minor":0, "digest":"a".repeat(64)});
        let resource = json!({"provider":"owner", "resource_id":"output", "incarnation":"native", "revision":2, "execution_domain":"seat", "mutation_domain":"topology", "mutation_domains":["topology"]});
        let method = json!({"service":"display/1", "method":"control", "interface_major":1, "interface_minor":0, "schema_revision":2});
        let proposal = json!({"proposal_id":"proposal", "output_id":"output-id", "owner_provider":"owner", "owner_service":"display/1", "target":target, "resource":resource, "method":method, "argument_schema":schema, "arguments":{"width":1920}, "access":"exclusive", "mutation_domains":["topology"]});
        plan.control_proposals
            .push(serde_json::from_value(proposal.clone()).unwrap());
        let context: ControlContext = serde_json::from_value(json!({
            "policy":{"dependent_provider":"display", "owner_provider":"owner", "owner_service":"display/1", "owner_method":method, "argument_schema":schema, "output_capability":"display", "output_schema":schema, "max_access":"exclusive", "mutation_domains":["topology"]},
            "output":{"output_id":"output-id", "capability":"display", "schema":schema, "producer":"owner", "producer_service":"display/1", "origin":target, "resource":resource, "data":{}},
            "target":target, "transition_id":"transition", "parent_generation":null
        })).unwrap();
        let mut state = AuthorityState::default();
        state.sequences.insert("owner".into(), 1);
        state
            .domain_owners
            .insert("topology".into(), "owner".into());
        state.snapshot.domains.insert(
            "topology".into(),
            DomainState {
                revision: 4,
                borrowers: [("stream".into(), 1)].into(),
            },
        );
        state.snapshot.resources.insert(
            "owner".into(),
            [(
                "output".into(),
                ResourceRef {
                    provider: "owner".into(),
                    resource_id: "output".into(),
                    incarnation: "native".into(),
                    revision: 2,
                    execution_domain: "seat".into(),
                    mutation_domains: ["topology".into()].into(),
                },
            )]
            .into(),
        );
        let mut authority =
            CoordinatorPrepareAuthority::new(state, vec![selected.clone()], Journal::default())
                .unwrap();
        let id = authority.admit_plan(&selected.participant, &plan).unwrap();
        let mut request: ResourceControlRequest = serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"operation","provider_sequence":1,"target":target,"proposal":proposal,"admission_id":id,"permit_id":"","remaining_ms":1000})).unwrap();
        let validate = |_: &SchemaRef, _: &Value| Ok(());
        authority
            .issue_control_permit(&selected.participant, &context, &mut request, validate)
            .unwrap();
        let mut retry = request.clone();
        retry.permit_id.clear();
        authority
            .issue_control_permit(&selected.participant, &context, &mut retry, validate)
            .unwrap();
        assert_eq!(retry.permit_id, request.permit_id);
        let mut wrong_origin = context.clone();
        wrong_origin.output.origin = serde_json::from_value(
            json!({"kind":"session","host_epoch":"host","session_id":"other","generation":1}),
        )
        .unwrap();
        assert!(!output_in_scope(&wrong_origin));
        let mut child = context.clone();
        child.target = serde_json::from_value(json!({"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"s","generation":3})).unwrap();
        child.parent_generation = Some(1);
        assert!(output_in_scope(&child));
        child.parent_generation = Some(2);
        assert!(!output_in_scope(&child));
        child.output.origin = serde_json::from_value(json!({"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"s","generation":2})).unwrap();
        assert!(!output_in_scope(&child));
        let mut forged = request.clone();
        forged.proposal.arguments["width"] = json!(1);
        assert!(
            authority
                .consume_control_permit("owner", &selected.participant, &context, &forged, validate)
                .is_err()
        );
        assert!(
            authority
                .consume_control_permit(
                    "foreign",
                    &selected.participant,
                    &context,
                    &request,
                    validate
                )
                .is_err()
        );
        let mut stale = context.clone();
        stale.output.resource.revision += 1;
        assert!(
            authority
                .consume_control_permit("owner", &selected.participant, &stale, &request, validate)
                .is_err()
        );
        assert_eq!(
            authority
                .consume_control_permit(
                    "owner",
                    &selected.participant,
                    &context,
                    &request,
                    validate
                )
                .unwrap(),
            ControlPermitUse::Execute
        );
        assert_eq!(
            authority
                .consume_control_permit(
                    "owner",
                    &selected.participant,
                    &context,
                    &request,
                    validate
                )
                .unwrap(),
            ControlPermitUse::ReattachOnly
        );
        assert!(
            authority
                .finish_admission("display", &request.admission_id)
                .is_err()
        );
        authority.settle_control_permit(&request).unwrap();
        authority
            .finish_admission("display", &request.admission_id)
            .unwrap();
    }

    #[test]
    fn durable_admission_freezes_obligations_and_blocks_conflicting_domain() {
        let selection = selection();
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal::default(),
        )
        .unwrap();
        let (sequence, _) = authority.allocate("display").unwrap();
        assert_eq!(sequence, 1);
        let admission = authority
            .admit_plan(&selection.participant, &plan())
            .unwrap();
        assert_eq!(authority.journal.snapshots.len(), 2);
        assert_eq!(
            authority.state.admissions[&admission]
                .plan
                .obligations
                .len(),
            1
        );
        assert!(
            authority
                .validate_rights(&selection.participant, &plan(), &admission)
                .is_ok()
        );
        assert!(
            authority
                .admit_plan(&selection.participant, &plan())
                .is_err()
        );
        let mut changed = plan();
        changed.mutation_domains[0]
            .borrowers
            .insert("other".into(), 9);
        assert!(
            authority
                .validate_rights(&selection.participant, &changed, &admission)
                .is_err()
        );
        let restored = CoordinatorPrepareAuthority::new(
            authority.state.clone(),
            vec![selection],
            Journal::default(),
        )
        .unwrap();
        assert_eq!(restored.state.sequences["display"], 1);
        assert!(restored.state.domain_locks.contains_key("topology"));
    }

    #[test]
    fn finish_retains_obligations_and_next_plan_refreshes_observations() {
        let mut selection = selection();
        selection.authorized_borrowers.insert("retained".into());
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal::default(),
        )
        .unwrap();
        let admission = authority
            .admit_plan(&selection.participant, &plan())
            .unwrap();
        assert!(authority.finish_admission("other", &admission).is_err());
        assert!(authority.state.domain_locks.contains_key("topology"));
        authority.finish_admission("display", &admission).unwrap();
        authority.finish_admission("display", &admission).unwrap();
        assert!(!authority.state.domain_locks.contains_key("topology"));
        assert!(authority.state.refresh_required.contains("topology"));
        assert_eq!(
            authority.state.settled[&admission].plan.obligations.len(),
            1
        );
        let mut changed = plan();
        changed.mutation_domains[0].expected_revision = 5;
        changed.mutation_domains[0]
            .borrowers
            .insert("retained".into(), 3);
        changed.resources[0].expected_revision = Some(3);
        assert!(
            authority
                .admit_plan(&selection.participant, &changed)
                .is_ok()
        );
        assert!(!authority.state.refresh_required.contains("topology"));
    }

    #[test]
    fn native_borrower_removal_requires_owner_fresh_revision_and_no_lock() {
        let selection = selection();
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal::default(),
        )
        .unwrap();
        let id = authority
            .admit_plan(&selection.participant, &plan())
            .unwrap();
        let old = authority.state.snapshot.domains["topology"].clone();
        assert!(
            authority
                .refresh_domain("display", "topology", old.clone())
                .is_err()
        );
        let mut locked_removal = old.clone();
        locked_removal.revision += 1;
        locked_removal.borrowers.remove("stream");
        assert!(
            authority
                .refresh_domain("display", "topology", locked_removal)
                .is_err()
        );
        authority.finish_admission("display", &id).unwrap();
        let mut stale = old.clone();
        stale.borrowers.remove("stream");
        assert!(
            authority
                .refresh_domain("display", "topology", stale)
                .is_err()
        );
        let mut current = old;
        current.revision += 1;
        current.borrowers.remove("stream");
        assert!(
            authority
                .refresh_domain("other", "topology", current.clone())
                .is_err()
        );
        assert!(
            authority
                .refresh_domain("display", "topology", current)
                .is_ok()
        );
        assert!(
            authority.state.snapshot.domains["topology"]
                .borrowers
                .is_empty()
        );
        assert_eq!(authority.state.settled[&id].plan.obligations.len(), 1);
        let mut resource = authority.state.snapshot.resources["display"]["output"].clone();
        resource.revision += 1;
        assert!(
            authority
                .refresh_resource("other", resource.clone())
                .is_err()
        );
        assert!(authority.refresh_resource("display", resource).is_ok());
        authority.allocate("display").unwrap();
        let poll = authority.allocate_poll_id("display").unwrap();
        assert_eq!(poll, "lifecycle-2");
        assert_eq!(authority.state.operation_counter, 2);
        assert_eq!(authority.state.sequences["display"], 1);
        CoordinatorPrepareAuthority::new(
            authority.state.clone(),
            vec![selection],
            Journal::default(),
        )
        .unwrap();
    }

    #[test]
    fn optional_binding_validation_requires_exact_approved_method() {
        let mut selected = selection();
        selected.participant.binding_validate_method = Some(method("binding_validate"));
        assert!(validate_selection(&selected).is_err());
        let selected_method = selected
            .participant
            .binding_validate_method
            .clone()
            .unwrap();
        selected.approvals.push(MethodApproval { method: selected_method.clone(), broker_approval: json!({"provider":"display", "method":{"service":selected_method.service,"method":selected_method.method,"interface_major":selected_method.interface_major,"interface_minor":selected_method.interface_minor}}) });
        assert!(validate_selection(&selected).is_ok());
        selected
            .approvals
            .last_mut()
            .unwrap()
            .method
            .schema_revision += 1;
        assert!(validate_selection(&selected).is_err());
    }

    #[test]
    fn bad_method_or_failed_journal_never_admits_work() {
        let selection = selection();
        let mut bad = selection.clone();
        bad.approvals[0].broker_approval["provider"] = json!("other");
        assert!(
            CoordinatorPrepareAuthority::new(
                AuthorityState::default(),
                vec![bad],
                Journal::default()
            )
            .is_err()
        );
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal {
                fail: true,
                ..Journal::default()
            },
        )
        .unwrap();
        assert!(
            authority
                .admit_plan(&selection.participant, &plan())
                .is_err()
        );
        assert!(authority.state.admissions.is_empty());
        authority.journal.fail = false;
        assert!(authority.allocate("display").is_err());
    }
    #[test]
    fn completed_empty_admissions_and_poll_ids_do_not_exhaust_capacity() {
        let selection = selection();
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal::default(),
        )
        .unwrap();
        let mut empty = plan();
        empty.resources.clear();
        empty.mutation_domains.clear();
        empty.obligations.clear();
        empty.required_capabilities.clear();
        for _ in 0..160 {
            let id = authority
                .admit_plan(&selection.participant, &empty)
                .unwrap();
            authority.finish_admission("display", &id).unwrap();
        }
        assert!(authority.state.admissions.is_empty());
        assert!(authority.state.settled.is_empty());
        let (sequence, first) = authority.allocate("display").unwrap();
        assert_eq!(sequence, 1);
        for _ in 0..4100 {
            authority.allocate_poll_id("display").unwrap();
        }
        assert!(authority.state.issued_operation_ids.is_empty());
        let mut recovered = CoordinatorPrepareAuthority::new(
            authority.state.clone(),
            vec![selection],
            Journal::default(),
        )
        .unwrap();
        let (sequence, next) = recovered.allocate("display").unwrap();
        assert_eq!(sequence, 2);
        assert_ne!(first, next);
        assert_eq!(next, "lifecycle-4102");
    }

    #[test]
    fn retained_admission_requires_complete_confirmed_release() {
        let selection = selection();
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal::default(),
        )
        .unwrap();
        let id = authority
            .admit_plan(&selection.participant, &plan())
            .unwrap();
        authority.finish_admission("display", &id).unwrap();
        for state in [
            CleanupState::Pending,
            CleanupState::DeferredRestoration,
            CleanupState::EffectUnknown,
        ] {
            let outcome = CleanupOutcome {
                obligation_id: "restore".into(),
                state,
                reason: None,
            };
            assert!(
                authority
                    .retire_settled("display", &id, &[outcome])
                    .is_err()
            );
            assert!(authority.state.settled.contains_key(&id));
        }
        assert!(authority.retire_settled("display", &id, &[]).is_err());
        let outcome = CleanupOutcome {
            obligation_id: "restore".into(),
            state: CleanupState::Released,
            reason: None,
        };
        assert!(
            authority
                .retire_settled("other", &id, std::slice::from_ref(&outcome))
                .is_err()
        );
        authority
            .retire_settled("display", &id, &[outcome])
            .unwrap();
        assert!(authority.state.settled.is_empty());
    }
    #[test]
    fn next_plan_accepts_fresh_provider_readback_without_retired_borrower() {
        let selection = selection();
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal::default(),
        )
        .unwrap();
        let id = authority
            .admit_plan(&selection.participant, &plan())
            .unwrap();
        let mut refreshed = plan();
        refreshed.mutation_domains[0].expected_revision += 1;
        refreshed.mutation_domains[0].borrowers.remove("stream");
        assert!(
            authority
                .admit_plan(&selection.participant, &refreshed)
                .is_err()
        );
        authority.finish_admission("display", &id).unwrap();
        let next = authority
            .admit_plan(&selection.participant, &refreshed)
            .unwrap();
        assert!(
            authority.state.snapshot.domains["topology"]
                .borrowers
                .is_empty()
        );
        assert!(authority.state.admissions.contains_key(&next));
        assert!(authority.state.settled.contains_key(&id));
    }

    #[test]
    fn observation_batch_publishes_complete_readback_in_one_write() {
        let selection = selection();
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal::default(),
        )
        .unwrap();
        let admission = authority
            .admit_plan(&selection.participant, &plan())
            .unwrap();
        authority.finish_admission("display", &admission).unwrap();
        let writes = authority.journal.snapshots.len();
        let mut domain = authority.state.snapshot.domains["topology"].clone();
        domain.revision += 1;
        domain.borrowers.insert("retained".into(), 3);
        let mut resource = authority.state.snapshot.resources["display"]["output"].clone();
        resource.revision += 1;
        resource.incarnation = "new-native-borrow".into();
        authority
            .refresh_observations(
                "display",
                &[("topology".into(), domain.clone())],
                std::slice::from_ref(&resource),
            )
            .unwrap();
        assert_eq!(authority.journal.snapshots.len(), writes + 1);
        assert_eq!(authority.state.snapshot.domains["topology"], domain);
        assert_eq!(
            authority.state.snapshot.resources["display"]["output"],
            resource
        );
        assert!(!authority.state.refresh_required.contains("topology"));
        assert!(authority.state.settled.contains_key(&admission));
    }

    #[test]
    fn observation_batch_rejects_invalid_tail_without_partial_publication() {
        let selection = selection();
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal::default(),
        )
        .unwrap();
        let admission = authority
            .admit_plan(&selection.participant, &plan())
            .unwrap();
        let old = authority.state.snapshot.domains["topology"].clone();
        assert!(
            authority
                .refresh_observations("display", &[("topology".into(), old.clone())], &[],)
                .is_err()
        );
        authority.finish_admission("display", &admission).unwrap();
        let writes = authority.journal.snapshots.len();
        let before = serde_json::to_value(&authority.state).unwrap();
        let mut domain = old.clone();
        domain.revision += 1;
        domain.borrowers.clear();
        let mut substituted = authority.state.snapshot.resources["display"]["output"].clone();
        substituted.revision += 1;
        substituted.execution_domain = "other-seat".into();
        assert!(
            authority
                .refresh_observations(
                    "display",
                    &[("topology".into(), domain.clone())],
                    &[substituted],
                )
                .is_err()
        );
        assert!(
            authority
                .refresh_observations("other", &[("topology".into(), domain.clone())], &[],)
                .is_err()
        );
        assert!(
            authority
                .refresh_observations(
                    "display",
                    &[
                        ("topology".into(), domain.clone()),
                        ("topology".into(), domain)
                    ],
                    &[],
                )
                .is_err()
        );
        let mut stale = old;
        stale.borrowers.clear();
        assert!(
            authority
                .refresh_observations("display", &[("topology".into(), stale)], &[],)
                .is_err()
        );
        assert_eq!(authority.journal.snapshots.len(), writes);
        assert_eq!(serde_json::to_value(&authority.state).unwrap(), before);
    }

    #[test]
    fn observation_batch_write_failure_retains_old_state_and_poison_fence() {
        let selection = selection();
        let mut authority = CoordinatorPrepareAuthority::new(
            AuthorityState::default(),
            vec![selection.clone()],
            Journal::default(),
        )
        .unwrap();
        let admission = authority
            .admit_plan(&selection.participant, &plan())
            .unwrap();
        authority.finish_admission("display", &admission).unwrap();
        let before = serde_json::to_value(&authority.state).unwrap();
        let mut domain = authority.state.snapshot.domains["topology"].clone();
        domain.revision += 1;
        authority.journal.fail = true;
        assert!(
            authority
                .refresh_observations("display", &[("topology".into(), domain.clone())], &[],)
                .is_err()
        );
        assert_eq!(serde_json::to_value(&authority.state).unwrap(), before);
        authority.journal.fail = false;
        assert!(
            authority
                .refresh_observations("display", &[("topology".into(), domain)], &[],)
                .is_err()
        );
    }
}

#[cfg(test)]
#[path = "prepare_materialization_tests.rs"]
mod materialization_tests;
