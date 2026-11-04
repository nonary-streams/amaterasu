//! Read-only Join adapter shared by built-in and independently built providers.
//! The caller authenticates the coordinator, source proof and admitted request.
use crate::*;
use serde_json::json;

pub fn validate_stream_join_context(
    context: &StreamJoinContext,
    target: &LifecycleTarget,
) -> Result<(), String> {
    let (
        LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation: 1..,
        },
        LifecycleTarget::Stream {
            host_epoch: source_epoch,
            session_id: source_session,
            stream_id: source_stream,
            generation,
        },
    ) = (target, &context.source)
    else {
        return Err("stream join requires distinct stream targets".into());
    };
    if host_epoch != source_epoch
        || session_id != source_session
        || stream_id == source_stream
        || *generation == 0
        || ![host_epoch, session_id, stream_id, source_stream]
            .into_iter()
            .all(|id| valid_opaque_id(id))
        || !valid_opaque_id(&context.source_transition)
    {
        return Err("invalid stream join context".into());
    }
    match (&context.binding_owner, &context.binding) {
        (Some(owner), Some(binding))
            if context.capture_binding_mode.is_managed()
                && valid_identifier(owner)
                && *generation == binding.generation =>
        {
            validate_capture_binding(binding)
        }
        (None, None) if context.capture_binding_mode.is_external() => Ok(()),
        _ => Err("join capture authority does not match the selected mode".into()),
    }
}

pub fn validate_stream_join_binding(
    context: &StreamJoinContext,
    target: &LifecycleTarget,
    binding: &CaptureBinding,
) -> Result<(), String> {
    validate_stream_join_context(context, target)?;
    let LifecycleTarget::Stream { generation, .. } = target else {
        return Err("stream target required".into());
    };
    let mut expected = context
        .binding
        .clone()
        .ok_or("external join has no managed binding")?;
    expected.generation = *generation;
    if binding != &expected {
        return Err("joined binding differs from source".into());
    }
    Ok(())
}

/// A Join never grants display, limiter or other source mutations. It keeps a
/// coordinator-owned reference to the already admitted source configuration.
pub fn stream_join_plan(request: &ProviderRequest, provider: &str) -> Result<ResourcePlan, String> {
    validate_provider_request(request)?;
    let context = request
        .join_context
        .as_ref()
        .ok_or("join context required")?;
    if request.intent != Some(StreamIntent::Join)
        || !valid_identifier(provider)
        || !request.control_results.is_empty()
    {
        return Err("invalid source join request".into());
    }
    let LifecycleTarget::Stream {
        host_epoch,
        session_id,
        stream_id: _,
        generation,
    } = &context.source
    else {
        return Err("invalid source join target".into());
    };
    if let Some(binding) = &context.binding {
        let [output] = request.dependency_inputs.as_slice() else {
            return Err("managed join requires source output".into());
        };
        if context.binding_owner.as_deref() != Some(output.producer.as_str())
            || output.resource != binding.resource
            || !matches!(&output.origin,LifecycleTarget::Session {host_epoch:epoch,session_id:session,..} if epoch==host_epoch && session==session_id)
            || !output
                .domain_observations
                .iter()
                .all(|observation| observation.complete)
            || *generation != binding.generation
        {
            return Err("source join proof mismatch".into());
        }
    } else if !request.dependency_inputs.is_empty() {
        return Err("external join cannot claim source output".into());
    }
    let owner = context.binding_owner.as_deref() == Some(provider);
    let plan = ResourcePlan {
        plan_id: format!("join-{}-{}", provider, request.transition_id),
        revision: context
            .binding
            .as_ref()
            .map_or(1, |binding| binding.plan_revision),
        resources: vec![],
        mutation_domains: vec![],
        required_capabilities: vec![STREAM_JOIN_CAPABILITY.into()],
        constraints: json!({(STREAM_JOIN_CAPABILITY):context}),
        obligations: vec![],
        affected_consumers: vec![],
        consumer_change: ConsumerChange::Live,
        candidate_constraint: context
            .binding
            .as_ref()
            .filter(|_| owner && request.phase == LifecyclePhase::StreamPrepare)
            .map(|binding| BindingConstraint {
                capture_provider: binding.capture_provider.clone(),
                selector_schema: binding.selector_schema.clone(),
                resource_provider: binding.resource.provider.clone(),
                resource_id: binding.resource.resource_id.clone(),
                incarnation: Some(binding.resource.incarnation.clone()),
                execution_domain: binding.resource.execution_domain.clone(),
                mode: Some(binding.mode.clone()),
                plan_revision: binding.plan_revision,
                geometry_revision: Some(binding.geometry_revision),
            }),
        allowed_degradation: vec![],
        control_proposals: vec![],
        resource_disposition: None,
    };
    validate_resource_plan(&plan)?;
    Ok(plan)
}

/// Decode the explicit no-mutation source lease contract carried by a Join
/// plan. This reference authorizes capture publication, never native mutation.
pub fn stream_join_plan_context(plan: &ResourcePlan) -> Result<Option<StreamJoinContext>, String> {
    let Some(value) = plan.constraints.get(STREAM_JOIN_CAPABILITY) else {
        return Ok(None);
    };
    let context: StreamJoinContext =
        serde_json::from_value(value.clone()).map_err(|_| "invalid shared source plan context")?;
    if !plan.resources.is_empty()
        || !plan.mutation_domains.is_empty()
        || !plan.obligations.is_empty()
        || !plan.control_proposals.is_empty()
        || plan.resource_disposition.is_some()
        || !plan.affected_consumers.is_empty()
        || !plan.allowed_degradation.is_empty()
        || plan.consumer_change != ConsumerChange::Live
        || plan.required_capabilities != [STREAM_JOIN_CAPABILITY.to_owned()]
        || context
            .binding
            .as_ref()
            .is_some_and(|binding| validate_capture_binding(binding).is_err())
        || context
            .binding_owner
            .as_deref()
            .is_some_and(|owner| !valid_identifier(owner))
        || !valid_opaque_id(&context.source_transition)
        || !matches!(
            &context.source,
            LifecycleTarget::Stream {
                generation: 1..,
                ..
            }
        )
        || plan.revision
            != context
                .binding
                .as_ref()
                .map_or(1, |binding| binding.plan_revision)
        || (context.capture_binding_mode.is_external()
            && (context.binding.is_some() || context.binding_owner.is_some()))
        || (context.capture_binding_mode.is_managed()
            && (context.binding.is_none() || context.binding_owner.is_none()))
        || plan
            .candidate_constraint
            .as_ref()
            .is_some_and(|constraint| !constraint_matches(&context, constraint))
    {
        return Err("shared source plan grants unsupported rights".into());
    }
    Ok(Some(context))
}

pub(crate) fn constraint_matches(
    context: &StreamJoinContext,
    constraint: &BindingConstraint,
) -> bool {
    let Some(binding) = &context.binding else {
        return false;
    };
    constraint.capture_provider == binding.capture_provider
        && constraint.selector_schema == binding.selector_schema
        && constraint.resource_provider == binding.resource.provider
        && constraint.resource_id == binding.resource.resource_id
        && constraint.incarnation.as_deref() == Some(binding.resource.incarnation.as_str())
        && constraint.execution_domain == binding.resource.execution_domain
        && constraint.mode.as_ref() == Some(&binding.mode)
        && constraint.plan_revision == binding.plan_revision
        && constraint.geometry_revision == Some(binding.geometry_revision)
}

/// No native effects or restoration obligations are created by this adapter.
/// Fresh native validation still occurs through the source provider's original
/// binding validation method before a joined consumer activates.
pub fn stream_join_callback(
    request: &ProviderRequest,
    provider: &str,
) -> Result<ProviderResponse, String> {
    if request.operation != ProviderOperation::LifecycleCallback {
        return Err("join callback operation required".into());
    }
    let expected = stream_join_plan(request, provider)?;
    if request.approved_plan.as_ref() != Some(&expected) {
        return Err("join plan changed".into());
    }
    let context = request
        .join_context
        .as_ref()
        .ok_or("join context required")?;
    let response = if request.phase == LifecyclePhase::StreamPrepare {
        let candidate = if context.binding_owner.as_deref() == Some(provider) {
            let LifecycleTarget::Stream { generation, .. } = &request.target else {
                return Err("invalid join target".into());
            };
            let mut binding = context
                .binding
                .clone()
                .ok_or("join source binding required")?;
            binding.generation = *generation;
            Some(binding)
        } else {
            None
        };
        ProviderResponse::Prepared {
            candidate,
            obligations: vec![],
            outputs: vec![],
        }
    } else {
        ProviderResponse::Cleanup {
            outcomes: vec![],
            outputs: vec![],
        }
    };
    validate_provider_response(request, &response)?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> ProviderRequest {
        let resource = json!({"provider":"owner","resource_id":"screen","incarnation":"native","revision":3,"execution_domain":"seat","mutation_domain":"topology","mutation_domains":["topology"]});
        let mode = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
        let binding = json!({"capture_provider":"capture","selector_schema":"selector/1","selector":{},"resource":resource,"mode":mode,"plan_revision":1,"geometry_revision":7,"generation":3});
        serde_json::from_value(json!({"version":2,"provider_sequence":1,"operation":"resource_plan","transition_id":"join-transition","operation_id":"join-plan","resource_generation":1,"target":{"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"subscriber","generation":2},"phase":"stream.prepare","settings_revision":"source-settings","settings":{},"intent":"join","reason":null,"requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},"source_video_mode":mode,"join_context":{"source":{"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"source","generation":3},"source_transition":"original-transition","binding_owner":"owner","binding":binding},"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null,"dependency_inputs":[{"output_id":"output","capability":"display/target","schema":{"id":"output/1","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"owner","producer_service":"owner/2","origin":{"kind":"session","host_epoch":"host","session_id":"session","generation":1},"resource":resource,"domain_observations":[{"domain":"topology","revision":3,"borrowers":{"session":1,"source":3},"complete":true}],"data":{"configured":true}}]})).unwrap()
    }
    #[test]
    fn unchanged_join_adapter_preserves_shared_binding_without_mutation_rights() {
        let mut request = request();
        validate_provider_request(&request).unwrap();
        let plan = stream_join_plan(&request, "owner").unwrap();
        assert!(plan.resources.is_empty());
        assert!(plan.mutation_domains.is_empty());
        assert!(plan.obligations.is_empty());
        request.operation = ProviderOperation::LifecycleCallback;
        request.admission_id = Some("admitted".into());
        request.plan_revision = Some(plan.revision);
        request.approved_plan = Some(plan);
        let ProviderResponse::Prepared {
            candidate: Some(binding),
            outputs,
            obligations,
        } = stream_join_callback(&request, "owner").unwrap()
        else {
            panic!("source owner publishes binding");
        };
        assert!(outputs.is_empty());
        assert!(obligations.is_empty());
        validate_stream_join_binding(
            request.join_context.as_ref().unwrap(),
            &request.target,
            &binding,
        )
        .unwrap();
        assert_eq!(binding.geometry_revision, 7);
        assert_eq!(binding.mode.width, 1920);
        assert_eq!(binding.generation, 2);
        let mut forged = binding;
        forged.geometry_revision += 1;
        assert!(
            validate_stream_join_binding(
                request.join_context.as_ref().unwrap(),
                &request.target,
                &forged
            )
            .is_err()
        );
    }
    #[test]
    fn rejects_forged_context_retargeted_resource_and_native_effect_plan() {
        let request = request();
        let mut wrong = request.clone();
        wrong.join_context.as_mut().unwrap().source_transition = String::new();
        assert!(stream_join_plan(&wrong, "owner").is_err());
        let mut wrong = request.clone();
        wrong
            .join_context
            .as_mut()
            .unwrap()
            .binding
            .as_mut()
            .unwrap()
            .generation += 1;
        assert!(stream_join_plan(&wrong, "owner").is_err());
        let mut wrong = request.clone();
        wrong.dependency_inputs[0].resource.incarnation = "other".into();
        assert!(stream_join_plan(&wrong, "owner").is_err());
        let mut plan = stream_join_plan(&request, "owner").unwrap();
        plan.affected_consumers.push("capture".into());
        assert!(validate_resource_plan(&plan).is_err());
        let mut missing = request;
        missing.join_context = None;
        assert!(validate_provider_request(&missing).is_err());
    }
}

#[cfg(test)]
mod external_tests {
    use super::*;
    #[test]
    fn external_v1_join_retains_policy_without_claiming_capture_authority() {
        let mut request:ProviderRequest=serde_json::from_value(json!({"version":1,"provider_sequence":1,"operation":"resource_plan","transition_id":"join-transition","operation_id":"join-plan","target":{"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"subscriber","generation":2},"phase":"stream.prepare","settings_revision":"source-settings","settings":{},"intent":"join","reason":null,"requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false},"join_context":{"source":{"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"source","generation":3},"source_transition":"source-transition","capture_binding_mode":"external","binding_owner":null,"binding":null},"consumer_requirements":{"execution_domain":"seat","capture_providers":[],"selector_schemas":[]},"remaining_ms":5000,"plan_revision":null,"admission_id":null,"approved_plan":null})).unwrap();
        let plan = stream_join_plan(&request, "host-policy").unwrap();
        assert!(plan.resources.is_empty());
        assert!(plan.candidate_constraint.is_none());
        assert!(plan.obligations.is_empty());
        request.operation = ProviderOperation::LifecycleCallback;
        request.admission_id = Some("admitted".into());
        request.plan_revision = Some(plan.revision);
        request.approved_plan = Some(plan);
        assert!(matches!(
            stream_join_callback(&request, "host-policy").unwrap(),
            ProviderResponse::Prepared {
                candidate: None,
                ..
            }
        ));
        request.join_context.as_mut().unwrap().binding_owner =
            Some("invented-capture-owner".into());
        assert!(validate_provider_request(&request).is_err());
    }
}
