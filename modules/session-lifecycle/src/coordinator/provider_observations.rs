//! Provider-scoped recovery and observation metadata.
use super::*;
use amaterasu_extension_api::{MutationDomainPlan, ResourceAccess};

const MAX_ITEMS: usize = 128;
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

/// This is called only after exact broker caller/request/admission verification.
/// Domain entries are the last authenticated observations; entries in
/// `stale_mutation_domains` must be refreshed before they are used as current.
pub(super) fn metadata(
    journal: &Journal,
    session: &SessionRecord,
    provider: &str,
) -> ModuleResult<Value> {
    let owned = |id: &String| {
        journal
            .authority
            .admissions
            .get(id)
            .or_else(|| journal.authority.settled.get(id))
            .filter(|admission| admission.provider == provider)
    };
    let mut selected_domains = BTreeSet::new();
    for id in &session.admission_ids {
        if let Some(admission) = owned(id) {
            selected_domains.extend(
                admission
                    .plan
                    .mutation_domains
                    .iter()
                    .map(|domain| domain.domain.clone()),
            );
        }
    }
    let mut admitted = Vec::new();
    let mut domains = BTreeMap::new();
    let mut target_values = BTreeSet::new();
    let mut obligations = Vec::new();
    for candidate_session in journal.sessions.values() {
        let is_origin = candidate_session.id == session.id;
        let mut included = false;
        for id in &candidate_session.admission_ids {
            let Some(admission) = owned(id) else {
                continue;
            };
            if !is_origin
                && !admission
                    .plan
                    .mutation_domains
                    .iter()
                    .any(|domain| selected_domains.contains(&domain.domain))
            {
                continue;
            }
            let target = candidate_session
                .admission_targets
                .get(id)
                .ok_or("admission_target_missing")?;
            let phase = candidate_session
                .admission_phases
                .get(id)
                .ok_or("admission_phase_missing")?;
            let (_, target_session, _, _) = super::operations::target_parts(target)?;
            if target_session != candidate_session.id {
                return Err("admission_target_mismatch".into());
            }
            for domain in &admission.plan.mutation_domains {
                if !selected_domains.contains(&domain.domain) {
                    continue;
                }
                if journal
                    .authority
                    .domain_owners
                    .get(&domain.domain)
                    .map(String::as_str)
                    != Some(provider)
                {
                    return Err("domain_not_owned".into());
                }
                let observed = journal
                    .authority
                    .snapshot
                    .domains
                    .get(&domain.domain)
                    .ok_or("domain_observation_missing")?;
                let entry = MutationDomainPlan {
                    domain: domain.domain.clone(),
                    expected_revision: observed.revision,
                    borrowers: observed.borrowers.clone(),
                    access: domain.access.clone(),
                };
                domains
                    .entry(domain.domain.clone())
                    .and_modify(|old: &mut MutationDomainPlan| {
                        if access_rank(&entry.access) > access_rank(&old.access) {
                            old.access = entry.access.clone();
                        }
                    })
                    .or_insert(entry);
            }
            target_values.insert(serde_json::to_string(target).map_err(|error| error.to_string())?);
            admitted.push(
                json!({"admission_id":id,"target":target,"phase":phase,"plan":admission.plan}),
            );
            included = true;
        }
        if included {
            obligations.extend(
                candidate_session
                    .obligations
                    .iter()
                    .filter(|obligation| {
                        obligation.provider == provider
                            && (is_origin
                                || obligation
                                    .mutation_domain
                                    .as_ref()
                                    .is_some_and(|domain| selected_domains.contains(domain)))
                    })
                    .cloned(),
            );
        }
    }
    let targets: Vec<Value> = target_values
        .into_iter()
        .map(|target| serde_json::from_str(&target).expect("serialized target is valid"))
        .collect();
    if admitted.len() > MAX_ITEMS
        || obligations.len() > MAX_ITEMS
        || domains.len() > MAX_ITEMS
        || targets.len() > MAX_ITEMS
    {
        return Err("provider_metadata_too_large".into());
    }
    let stale: Vec<_> = domains
        .keys()
        .filter(|domain| journal.authority.refresh_required.contains(*domain))
        .cloned()
        .collect();
    let value = json!({"admitted_plans":admitted,"obligations":obligations,"mutation_domains":domains.into_values().collect::<Vec<_>>(),"stale_mutation_domains":stale,"resource_targets":targets});
    if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > MAX_RESPONSE_BYTES {
        return Err("provider_metadata_too_large".into());
    }
    Ok(value)
}

fn access_rank(access: &ResourceAccess) -> u8 {
    match access {
        ResourceAccess::Read => 0,
        ResourceAccess::Shared => 1,
        ResourceAccess::Exclusive => 2,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DomainObservation {
    domain: String,
    revision: u64,
    borrowers: BTreeMap<String, u64>,
}

struct StagedAuthority(Option<crate::prepare_authority::AuthorityState>);
impl crate::prepare_authority::AuthorityJournal for StagedAuthority {
    fn persist(&mut self, state: &crate::prepare_authority::AuthorityState) -> ModuleResult<()> {
        self.0 = Some(state.clone());
        Ok(())
    }
}

impl Coordinator {
    /// Authenticate the provider and atomically replace only existing, unlocked
    /// observations covered by its own admitted plans for this session.
    pub(super) fn refresh_provider_observations(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: Value,
        key: String,
    ) -> ModuleResult<Value> {
        if !self
            .settings
            .participants
            .iter()
            .any(|participant| participant.provider == caller)
        {
            return Err("provider_not_selected".into());
        }
        let target: LifecycleTarget =
            serde_json::from_value(payload.get("target").cloned().ok_or("target_required")?)
                .map_err(|_| "invalid_target")?;
        let (epoch, session_id, stream_id, generation) = super::operations::target_parts(&target)?;
        let mut next = self.shared.borrow().value.clone();
        let session = next.sessions.get(session_id).ok_or("unknown_session")?;
        if epoch != next.host_epoch || epoch != session.host_epoch {
            return Err("stale_host_epoch".into());
        }
        if let Some(stream_id) = stream_id {
            if session
                .streams
                .get(stream_id)
                .is_none_or(|stream| stream.generation != generation)
            {
                return Err("stale_generation".into());
            }
        } else if session.generation != generation {
            return Err("stale_generation".into());
        }
        let mut allowed_domains = BTreeSet::new();
        let mut allowed_resources = BTreeSet::new();
        for id in &session.admission_ids {
            let Some(admission) = next
                .authority
                .admissions
                .get(id)
                .or_else(|| next.authority.settled.get(id))
            else {
                continue;
            };
            if admission.provider != caller {
                continue;
            }
            let original_target = session
                .admission_targets
                .get(id)
                .ok_or("admission_target_missing")?;
            if super::operations::target_parts(original_target)?.1 != session_id {
                return Err("admission_target_mismatch".into());
            }
            for domain in &admission.plan.mutation_domains {
                allowed_domains.insert(domain.domain.clone());
            }
            for resource in &admission.plan.resources {
                if resource.provider == caller {
                    allowed_resources.insert(resource.resource_id.clone());
                }
            }
        }
        if allowed_domains.is_empty() && allowed_resources.is_empty() {
            return Err("provider_session_not_owned".into());
        }
        let domains: Vec<DomainObservation> =
            serde_json::from_value(payload.get("domains").cloned().ok_or("domains_required")?)
                .map_err(|_| "invalid_domains")?;
        let resources: Vec<crate::planning::ResourceRef> = serde_json::from_value(
            payload
                .get("resources")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|_| "invalid_resources")?;
        if domains.len() > MAX_ITEMS
            || resources.len() > MAX_ITEMS
            || (domains.is_empty() && resources.is_empty())
        {
            return Err("invalid_observation_batch".into());
        }
        if domains
            .iter()
            .any(|domain| !allowed_domains.contains(&domain.domain))
            || resources.iter().any(|resource| {
                resource.provider != caller || !allowed_resources.contains(&resource.resource_id)
            })
        {
            return Err("observation_not_owned_by_session".into());
        }
        let observations: Vec<_> = domains
            .into_iter()
            .map(|domain| {
                (
                    domain.domain,
                    crate::planning::DomainState {
                        revision: domain.revision,
                        borrowers: domain.borrowers,
                    },
                )
            })
            .collect();
        let selections: Vec<_> = self.selections.values().cloned().collect();
        let mut authority = crate::prepare_authority::CoordinatorPrepareAuthority::new(
            next.authority.clone(),
            selections,
            StagedAuthority(None),
        )?;
        authority.refresh_observations(caller, &observations, &resources)?;
        next.authority = authority.state().clone();
        let refreshed = metadata(
            &next,
            next.sessions.get(session_id).ok_or("unknown_session")?,
            caller,
        )?;
        let mut response =
            json!({"status":"succeeded","operation_id":payload["operation_id"],"target":target});
        response
            .as_object_mut()
            .unwrap()
            .extend(refreshed.as_object().ok_or("invalid_metadata")?.clone());
        if serde_json::to_vec(&response)
            .map_err(|e| e.to_string())?
            .len()
            > MAX_RESPONSE_BYTES
        {
            return Err("provider_metadata_too_large".into());
        }
        next.receipts.insert(
            key,
            Receipt {
                caller: caller.into(),
                initiator: initiator.into(),
                request: payload,
                response: response.clone(),
                work: None,
                wait_target: None,
            },
        );
        save(&self.shared, next)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "provider_observations_tests.rs"]
mod tests;
