//! Resolve signed service declarations against installed startup policy.
//! Declarations describe trusted native code; they do not attest its behavior.
use std::collections::BTreeSet;

use amaterasu_contracts::{ApiPrivilege, RuntimeConfig, ServiceSelections, valid_name};

use crate::{
    ComponentManifest, PackageResult, ServiceDiagnostic, ServiceOffer, ServiceRequirement,
    ServiceTransport,
};

const MAX_DECLARATIONS: usize = 256;

pub(super) fn resolve(
    components: &[ComponentManifest],
    policy: &ServiceSelections,
    runtime: &mut RuntimeConfig,
) -> PackageResult<Vec<ServiceDiagnostic>> {
    let mut selections = policy.clone();
    let mut diagnostics = Vec::new();
    for component in components {
        if component.service_provides.len() > MAX_DECLARATIONS
            || component.service_requires.len() > MAX_DECLARATIONS
            || (component.runtime
                && (!component.service_provides.is_empty()
                    || !component.service_requires.is_empty()))
        {
            return Err(format!("invalid service declarations for {}", component.id));
        }
        let mut offers = BTreeSet::new();
        for offer in &component.service_provides {
            reject_raw_api_collision(offer.transport, &offer.service, runtime)?;
            validate_descriptor(
                offer.transport,
                &offer.service,
                &offer.contract,
                offer.method.as_deref(),
                offer.major,
                &offer.capabilities,
            )?;
            if !offers.insert((offer.transport, &offer.service, &offer.method)) {
                return Err(format!("duplicate service offer for {}", component.id));
            }
        }
        let mut requirements = BTreeSet::new();
        for requirement in &component.service_requires {
            reject_raw_api_collision(requirement.transport, &requirement.service, runtime)?;
            validate_descriptor(
                requirement.transport,
                &requirement.service,
                &requirement.contract,
                requirement.method.as_deref(),
                requirement.major,
                &requirement.capabilities,
            )?;
            if !requirements.insert((
                requirement.transport,
                &requirement.service,
                &requirement.method,
            )) {
                return Err(format!(
                    "duplicate service requirement for {}",
                    component.id
                ));
            }
        }
    }
    if policy.broker.len() > MAX_DECLARATIONS || policy.native.len() > MAX_DECLARATIONS {
        return Err("too many service selections".into());
    }
    for (native, entries) in [(false, &policy.broker), (true, &policy.native)] {
        for (service, choice) in entries {
            if !valid_name(service) || service == amaterasu_contracts::HOST_LOG_SERVICE {
                return Err(format!("invalid selected service: {service}"));
            }
            let Some(choice) = choice else { continue };
            let provider = components
                .iter()
                .find(|component| !component.runtime && component.id == choice.provider)
                .ok_or_else(|| {
                    format!(
                        "selected provider {} for {service} is absent",
                        choice.provider
                    )
                })?;
            if !valid_name(&choice.contract)
                || !has_grant(runtime, &provider.id, service, native, true)
                || !provider.service_provides.iter().any(|offer| {
                    offer.service == *service
                        && offer.contract == choice.contract
                        && (offer.transport == ServiceTransport::Native) == native
                })
            {
                return Err(format!(
                    "invalid or ungranted selected provider {} for {service}",
                    provider.id
                ));
            }
        }
    }
    for consumer in components {
        for requirement in &consumer.service_requires {
            let native = requirement.transport == ServiceTransport::Native;
            let entries = if native {
                &mut selections.native
            } else {
                &mut selections.broker
            };
            let selected = entries.entry(requirement.service.clone()).or_insert(None);
            let Some(selected) = selected else {
                if !requirement.optional {
                    return Err(format!(
                        "required service {} for {} is not selected",
                        requirement.service, consumer.id
                    ));
                }
                diagnostics.push(ServiceDiagnostic {
                    consumer: consumer.id.clone(),
                    service: requirement.service.clone(),
                    transport: requirement.transport,
                });
                continue;
            };
            let provider = components
                .iter()
                .find(|component| component.id == selected.provider)
                .ok_or_else(|| format!("selected provider {} is absent", selected.provider))?;
            if !has_grant(runtime, &consumer.id, &requirement.service, native, false) {
                return Err(format!(
                    "consumer {} lacks grant for {}",
                    consumer.id, requirement.service
                ));
            }
            if consumer.id != provider.id
                && consumer.requires.get(&provider.id) != Some(&provider.version)
            {
                return Err(format!(
                    "consumer {} lacks dependency on selected provider {} {}",
                    consumer.id, provider.id, provider.version
                ));
            }
            let offer = provider
                .service_provides
                .iter()
                .find(|offer| matches_requirement(offer, requirement, &selected.contract))
                .ok_or_else(|| {
                    format!(
                        "incompatible service {} for {}",
                        requirement.service, consumer.id
                    )
                })?;
            if requirement.transport == ServiceTransport::Api {
                let approved = runtime.api_methods.iter().any(|approval| {
                    approval.provider == provider.id
                        && approval.callers.contains(&consumer.id)
                        && approval.method.service == offer.service
                        && Some(approval.method.method.as_str()) == offer.method.as_deref()
                        && approval.method.interface_major == offer.major
                        && approval.method.interface_minor == requirement.minor
                        && approval.method.privilege == ApiPrivilege::User
                });
                if !approved {
                    return Err(format!(
                        "API approval missing for {} on {}",
                        consumer.id, requirement.service
                    ));
                }
            }
        }
    }
    if selections.broker.len() > MAX_DECLARATIONS || selections.native.len() > MAX_DECLARATIONS {
        return Err("too many resolved service selections".into());
    }
    for approval in &runtime.api_methods {
        if let Some(Some(selection)) = selections.broker.get(&approval.method.service)
            && selection.provider != approval.provider
        {
            return Err(format!(
                "service selection conflicts with API approval for {}",
                approval.method.service
            ));
        }
    }
    runtime.service_selections = selections;
    Ok(diagnostics)
}

fn reject_raw_api_collision(
    transport: ServiceTransport,
    service: &str,
    runtime: &RuntimeConfig,
) -> PackageResult<()> {
    if transport == ServiceTransport::Raw
        && runtime
            .api_methods
            .iter()
            .any(|approval| approval.method.service == service)
    {
        return Err(format!(
            "raw service {service} is reserved by installed API approvals"
        ));
    }
    Ok(())
}

fn validate_descriptor(
    transport: ServiceTransport,
    service: &str,
    contract: &str,
    method: Option<&str>,
    major: u32,
    capabilities: &BTreeSet<String>,
) -> PackageResult<()> {
    if !valid_name(service)
        || !valid_name(contract)
        || major == 0
        || capabilities.len() > 64
        || capabilities
            .iter()
            .any(|capability| !valid_name(capability))
        || match (transport, method) {
            (ServiceTransport::Api, Some(method)) => !valid_name(method) || method.contains('/'),
            (ServiceTransport::Api, None) => true,
            (_, Some(_)) => true,
            (_, None) => false,
        }
    {
        return Err(format!("invalid service declaration for {service}"));
    }
    Ok(())
}

fn matches_requirement(
    offer: &ServiceOffer,
    requirement: &ServiceRequirement,
    selected_contract: &str,
) -> bool {
    offer.transport == requirement.transport
        && offer.service == requirement.service
        && offer.contract == requirement.contract
        && offer.contract == selected_contract
        && offer.method == requirement.method
        && offer.major == requirement.major
        && if requirement.transport == ServiceTransport::Native {
            offer.minor >= requirement.minor
        } else {
            offer.minor == requirement.minor
        }
        && requirement.capabilities.is_subset(&offer.capabilities)
}

fn has_grant(
    runtime: &RuntimeConfig,
    principal: &str,
    service: &str,
    native: bool,
    provide: bool,
) -> bool {
    runtime
        .principals
        .get(principal)
        .is_some_and(|grants| match (native, provide) {
            (true, true) => grants.native_provide.contains(service),
            (true, false) => grants.native_use.contains(service),
            (false, true) => grants.provide.contains(service),
            (false, false) => grants.call.contains(service),
        })
}
