//! Serialized extension contracts. References select resources; they never confer authority.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SESSION_LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";
pub const EXTENSION_REGISTRY_SERVICE: &str = "amaterasu.extension-registry/1";
pub const EXTENSION_ACTIONS_SERVICE: &str = "amaterasu.extension-actions/1";
pub const CONTRACT_VERSION: u32 = 1;

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
    pub incarnation: String,
    pub revision: u64,
    pub execution_domain: String,
    pub mutation_domain: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureBinding {
    pub capture_provider: String,
    pub selector_schema: String,
    pub selector: Value,
    pub resource: ResourceRef,
    pub mode: VideoMode,
    pub plan_revision: u64,
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
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
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
        let value = json!({"capture_provider":"capture", "selector_schema":"selector/1", "selector":{}, "resource":{"provider":"display", "resource_id":"resource", "incarnation":"instance", "revision":1, "execution_domain":"desktop", "mutation_domain":null}, "mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}, "plan_revision":3});
        let binding: CaptureBinding = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(binding.fallback, CaptureFallback::Forbidden);
        let mut unsupported = value;
        unsupported["fallback"] = json!("approved");
        assert!(serde_json::from_value::<CaptureBinding>(unsupported).is_err());
    }
}
