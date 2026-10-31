//! Validation after discovery has authenticated the installed descriptor.
use amaterasu_configuration_schema::{Values, validate_values};
use amaterasu_extension_api::{
    ActionDescriptor, ActionInvoke, ActionTarget, ExecutionClass, TargetKind,
};

/// Checks request consistency and argument schema. The caller must separately
/// verify installed approvals, identity, target ownership, and lifecycle permit.
pub fn validate_request(action: &ActionDescriptor, invoke: &ActionInvoke) -> Result<(), String> {
    if action.id != invoke.action_id {
        return Err("selected descriptor does not match action".into());
    }
    if invoke.operation_id != format!("{}:{}", invoke.epoch, invoke.sequence)
        || invoke.sequence == 0
    {
        return Err("invalid action operation id".into());
    }
    let kind = match &invoke.target {
        ActionTarget::Session { .. } => TargetKind::Session,
        ActionTarget::Stream { .. } => TargetKind::Stream,
        ActionTarget::Host { .. } => TargetKind::Host,
    };
    if !action.allowed_targets.contains(&kind) {
        return Err("target is not declared for this action".into());
    }
    match (&action.execution, &kind) {
        (ExecutionClass::SessionMutation, TargetKind::Host)
        | (ExecutionClass::HostMutation, TargetKind::Session | TargetKind::Stream) => {
            return Err("target does not match mutation scope".into());
        }
        _ => {}
    }
    if action.execution != ExecutionClass::ReadOnly && action.plan.is_none() {
        return Err("mutating action has no declared plan method".into());
    }
    let object = invoke
        .arguments
        .as_object()
        .ok_or("arguments must be an object")?;
    let values: Values = object
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    validate_values(&action.arguments, &values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_configuration_schema::Definition;
    use amaterasu_extension_api::MethodRef;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn fixture() -> (ActionDescriptor, ActionInvoke) {
        let method = MethodRef {
            service: "example.provider/1".into(),
            method: "action_execute".into(),
            interface_major: 1,
            interface_minor: 0,
            schema_revision: 1,
        };
        let action = ActionDescriptor {
            id: "example.provider/change".into(),
            label: "Change".into(),
            description: "Change".into(),
            execution: ExecutionClass::SessionMutation,
            arguments: BTreeMap::from([("enabled".into(), Definition::Boolean { default: false })]),
            allowed_targets: vec![TargetKind::Session],
            required_capabilities: vec![],
            plan: Some(method.clone()),
            execute: method,
            status: None,
        };
        let invoke = ActionInvoke {
            epoch: "abc".into(),
            sequence: 1,
            operation_id: "abc:1".into(),
            action_id: action.id.clone(),
            target: ActionTarget::Session {
                host_epoch: "host".into(),
                session_id: "session".into(),
                expected_generation: 2,
            },
            arguments: json!({"enabled":true}),
            expected_settings_revision: None,
        };
        (action, invoke)
    }

    #[test]
    fn validates_schema_and_requires_a_plan_for_mutation() {
        let (mut action, mut invoke) = fixture();
        assert!(validate_request(&action, &invoke).is_ok());
        invoke.arguments = json!({"enabled":true,"extra":42});
        assert!(validate_request(&action, &invoke).is_err());
        invoke.arguments = json!({"enabled":"yes"});
        assert!(validate_request(&action, &invoke).is_err());
        invoke.arguments = json!({"enabled":true});
        action.plan = None;
        assert!(validate_request(&action, &invoke).is_err());
    }

    #[test]
    fn rejects_retargeting_scope_and_noncanonical_identity() {
        let (mut action, mut invoke) = fixture();
        invoke.operation_id = "abc:2".into();
        assert!(validate_request(&action, &invoke).is_err());
        invoke.operation_id = "abc:1".into();
        invoke.target = ActionTarget::Host {
            host_epoch: "host".into(),
        };
        action.allowed_targets.push(TargetKind::Host);
        assert!(validate_request(&action, &invoke).is_err());
        action.execution = ExecutionClass::HostMutation;
        assert!(validate_request(&action, &invoke).is_ok());
    }
}
