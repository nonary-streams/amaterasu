//! Provider-neutral environment edits for an independently selected launch consumer.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const LAUNCH_ENVIRONMENT_CAPABILITY: &str = "amaterasu.launch-environment";
/// The decoder additionally enforces bounded names, values and extensions.
pub const LAUNCH_ENVIRONMENT_SCHEMA_DOCUMENT: &str = r#"{"type":"object","required":["edits"],"properties":{"edits":{"type":"object"},"extensions":{"type":"object"}},"additionalProperties":false}"#;

pub fn launch_environment_schema() -> crate::SchemaRef {
    crate::SchemaRef {
        id: LAUNCH_ENVIRONMENT_CAPABILITY.into(),
        major: 1,
        minor: 0,
        digest: format!(
            "{:x}",
            Sha256::digest(LAUNCH_ENVIRONMENT_SCHEMA_DOCUMENT.as_bytes())
        ),
    }
}

/// A selected lifecycle participant's requested launch-environment edits.
/// This is data only: a consumer must authenticate its coordinator-selected
/// origin and target before accepting it. `None` removes a variable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchEnvironmentContribution {
    pub edits: std::collections::BTreeMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, EnvironmentExtension>,
}

/// Extend a delimited value without knowing its inherited content. Prefix and
/// suffix entries are whole tokens; existing tokens retain their order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentExtension {
    pub separator: char,
    pub prefix: Vec<String>,
    pub suffix: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    pub deduplicate: bool,
}

/// An agreed constraint from an authenticated participant's completed preparation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedPlan {
    pub provider: String,
    pub plan: crate::ResourcePlan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedPlansRequest {
    pub operation: String,
    pub target: crate::LifecycleTarget,
    pub owner: String,
    pub application_operation_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedPlansResponse {
    pub status: String,
    pub target: crate::LifecycleTarget,
    pub plans: Vec<PreparedPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchEnvironmentRequirement {
    pub schema: crate::SchemaRef,
    pub contribution: LaunchEnvironmentContribution,
}

/// Interpret only this consumer's constraint, after authenticating the selected
/// coordinator and target. Other plugin constraints remain opaque to the launcher.
pub fn launch_environment_contributions(
    plans: &[PreparedPlan],
) -> Result<Vec<LaunchEnvironmentContribution>, String> {
    if plans.len() > 32 {
        return Err("too many prepared plans".into());
    }
    let mut result = Vec::new();
    for prepared in plans {
        crate::validate_resource_plan(&prepared.plan)?;
        let requirement = prepared.plan.constraints.get("launch_environment");
        let granted = prepared
            .plan
            .required_capabilities
            .iter()
            .any(|value| value == LAUNCH_ENVIRONMENT_CAPABILITY);
        match (granted, requirement) {
            (false, None) => continue,
            (true, Some(value)) => {
                let requirement: LaunchEnvironmentRequirement =
                    serde_json::from_value(value.clone())
                        .map_err(|_| "invalid launch environment constraint")?;
                if requirement.schema != launch_environment_schema() {
                    return Err("incompatible launch environment schema".into());
                }
                result.push(requirement.contribution);
            }
            _ => return Err("ungranted or missing launch environment constraint".into()),
        }
    }
    // Validate every edit even when the caller has no inherited environment.
    merge_launch_environment(&BTreeMap::new(), &result)?;
    Ok(result)
}

/// Merge already-authorized contributions with an application environment.
/// Independent participants cannot silently override each other's keys. The
/// limits match the application launch service and are checked before spawn.
pub fn merge_launch_environment(
    base: &std::collections::BTreeMap<String, String>,
    contributions: &[LaunchEnvironmentContribution],
) -> Result<std::collections::BTreeMap<String, String>, String> {
    if contributions.len() > 32 {
        return Err("too many environment contributions".into());
    }
    let mut result = base.clone();
    let mut claimed = std::collections::BTreeSet::new();
    for contribution in contributions {
        if contribution.edits.len() + contribution.extensions.len() > 32 {
            return Err("too many environment edits".into());
        }
        for (name, extension) in &contribution.extensions {
            if !valid_environment_name(name) || !claimed.insert(name.to_ascii_uppercase()) {
                return Err("invalid or conflicting environment edit".into());
            }
            if result
                .keys()
                .any(|existing| existing != name && existing.eq_ignore_ascii_case(name))
            {
                return Err("environment name case conflict".into());
            }
            let value = extend_value(result.get(name).map(String::as_str), extension)?;
            result.insert(name.clone(), value);
        }
        for (name, value) in &contribution.edits {
            if !valid_environment_name(name) || !claimed.insert(name.to_ascii_uppercase()) {
                return Err("invalid or conflicting environment edit".into());
            }
            if result
                .keys()
                .any(|existing| existing != name && existing.eq_ignore_ascii_case(name))
            {
                return Err("environment name case conflict".into());
            }
            match value {
                Some(value) if value.len() <= 1024 && !value.contains('\0') => {
                    result.insert(name.clone(), value.clone());
                }
                Some(_) => return Err("invalid environment value".into()),
                None => {
                    result.remove(name);
                }
            }
        }
    }
    let mut names = std::collections::BTreeSet::new();
    if result.len() > 32
        || result.iter().any(|(name, value)| {
            !valid_environment_name(name)
                || !names.insert(name.to_ascii_uppercase())
                || value.len() > 1024
                || value.contains('\0')
        })
        || result
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
            > 4096
    {
        return Err("invalid or oversized launch environment".into());
    }
    Ok(result)
}

fn extend_value(base: Option<&str>, extension: &EnvironmentExtension) -> Result<String, String> {
    if !extension.separator.is_ascii()
        || extension.separator.is_ascii_control()
        || extension.prefix.len() + extension.suffix.len() + extension.remove.len() > 32
        || extension
            .prefix
            .iter()
            .chain(&extension.suffix)
            .chain(&extension.remove)
            .any(|part| {
                part.is_empty()
                    || part.len() > 1024
                    || part.contains('\0')
                    || part.contains(extension.separator)
            })
    {
        return Err("invalid environment extension".into());
    }
    let mut seen = BTreeSet::new();
    let parts: Vec<_> = extension
        .prefix
        .iter()
        .map(String::as_str)
        .chain(
            base.into_iter()
                .flat_map(|value| value.split(extension.separator)),
        )
        .chain(extension.suffix.iter().map(String::as_str))
        .filter(|part| !part.is_empty())
        .filter(|part| !extension.remove.iter().any(|removed| removed == part))
        .filter(|part| !extension.deduplicate || seen.insert(*part))
        .collect();
    Ok(parts.join(&extension.separator.to_string()))
}

fn valid_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn requirement() -> PreparedPlan {
        serde_json::from_value(json!({"provider":"replaceable.plugin","plan":{
            "plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],
            "required_capabilities":[LAUNCH_ENVIRONMENT_CAPABILITY],
            "constraints":{"launch_environment":{"schema":launch_environment_schema(),
                "contribution":{"edits":{"EXAMPLE":"value"}}}},
            "obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,
            "allowed_degradation":[]}})).unwrap()
    }

    #[test]
    fn known_schema_and_explicit_capability_are_both_required() {
        let mut prepared = requirement();
        assert_eq!(
            launch_environment_contributions(&[prepared.clone()]).unwrap()[0].edits["EXAMPLE"],
            Some("value".into())
        );
        prepared.plan.required_capabilities.clear();
        assert!(launch_environment_contributions(&[prepared]).is_err());
        let mut prepared = requirement();
        prepared.plan.constraints["launch_environment"]["schema"]["digest"] = json!("f".repeat(64));
        assert!(launch_environment_contributions(&[prepared]).is_err());
        let mut unrelated = requirement();
        unrelated.plan.required_capabilities.clear();
        unrelated.plan.constraints = json!({"foreign_feature":{"data":true}});
        assert!(
            launch_environment_contributions(&[unrelated])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn extensions_preserve_order_remove_exact_tokens_and_obey_final_bounds() {
        let base = BTreeMap::from([("LIST".into(), "one:remove:two:one".into())]);
        let contribution = LaunchEnvironmentContribution {
            edits: BTreeMap::new(),
            extensions: BTreeMap::from([(
                "LIST".into(),
                EnvironmentExtension {
                    separator: ':',
                    prefix: vec!["first".into()],
                    suffix: vec!["two".into(), "last".into()],
                    remove: vec!["remove".into()],
                    deduplicate: true,
                },
            )]),
        };
        let result = merge_launch_environment(&base, std::slice::from_ref(&contribution)).unwrap();
        assert_eq!(result["LIST"], "first:one:two:last");
        assert_eq!(base["LIST"], "one:remove:two:one");
        let mut invalid = contribution.clone();
        invalid.extensions.get_mut("LIST").unwrap().suffix = vec!["injected:token".into()];
        assert!(merge_launch_environment(&base, &[invalid]).is_err());
        let mut oversized = contribution;
        oversized.extensions.get_mut("LIST").unwrap().suffix = vec!["x".repeat(1024)];
        assert!(merge_launch_environment(&base, &[oversized]).is_err());
    }
}
