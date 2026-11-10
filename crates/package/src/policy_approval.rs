//! Exact, host-authorized bootstrap policy changes for signed package trials.
//!
//! `PolicyApproval` is an authority record, not a claim from a package. The host
//! must load it from independently protected, administrator-controlled state,
//! obtain the context from verified publisher/manifest and locked selection
//! state, and revalidate those facts at commit. Native modules are trusted code;
//! runtime grants are routing permissions, not a native-code sandbox.
use crate::{BootstrapConfig, PackageResult, sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_BOOTSTRAP_BYTES: usize = 1024 * 1024;
const MAX_CHANGES: usize = 256;

/// A missing property is distinct from a property explicitly set to null.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PolicyValue {
    Absent,
    Present(Value),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedPolicyChange {
    pub before: PolicyValue,
    pub after: PolicyValue,
}

/// An exact one-transaction authorization, issued by the host's protected
/// administrative authority. A publisher or candidate cannot approve itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyApproval {
    pub source_bootstrap_sha256: String,
    pub candidate_bootstrap_sha256: String,
    pub selection_revision: String,
    pub publisher_fingerprint: String,
    pub manifest_sha256: String,
    pub changes: BTreeMap<String, ApprovedPolicyChange>,
}

/// Independently provisioned host allowlist. This must be loaded only from the
/// protected host policy, never a package, proposal, or management request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAllowlist {
    pub publisher_fingerprint: String,
    pub changes: BTreeMap<String, ApprovedPolicyChange>,
}

/// Observed by the host from protected selection and a verified package, not
/// from the package's request or from the approval record itself.
pub struct PolicyCandidateContext<'a> {
    pub selection_revision: &'a str,
    pub publisher_fingerprint: &'a str,
    pub manifest_sha256: &'a str,
}

/// Verify exact changes before a trial; this does not publish policy or select a
/// package. The caller retains the lock, verifies approval provenance, and
/// coordinates the policy and package state transaction with rollback.
pub fn verify_policy_candidate(
    allowlist: &PolicyAllowlist,
    approval: &PolicyApproval,
    context: &PolicyCandidateContext<'_>,
    current_bytes: &[u8],
    candidate_bytes: &[u8],
) -> PackageResult<BootstrapConfig> {
    if allowlist.publisher_fingerprint != approval.publisher_fingerprint
        || allowlist.changes.len() > MAX_CHANGES
        || approval
            .changes
            .iter()
            .any(|(path, change)| allowlist.changes.get(path) != Some(change))
    {
        return Err("policy change exceeds protected host allowlist".into());
    }
    verify_exact_policy_candidate(approval, context, current_bytes, candidate_bytes)
}

fn verify_exact_policy_candidate(
    approval: &PolicyApproval,
    context: &PolicyCandidateContext<'_>,
    current_bytes: &[u8],
    candidate_bytes: &[u8],
) -> PackageResult<BootstrapConfig> {
    if current_bytes.len() > MAX_BOOTSTRAP_BYTES || candidate_bytes.len() > MAX_BOOTSTRAP_BYTES {
        return Err("bootstrap exceeds policy approval limit".into());
    }
    for digest in [
        approval.source_bootstrap_sha256.as_str(),
        approval.candidate_bootstrap_sha256.as_str(),
        approval.selection_revision.as_str(),
        approval.publisher_fingerprint.as_str(),
        approval.manifest_sha256.as_str(),
    ] {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err("policy approval contains an invalid digest".into());
        }
    }
    if approval.source_bootstrap_sha256 != sha256(current_bytes)
        || approval.candidate_bootstrap_sha256 != sha256(candidate_bytes)
        || approval.selection_revision != context.selection_revision
        || approval.publisher_fingerprint != context.publisher_fingerprint
        || approval.manifest_sha256 != context.manifest_sha256
    {
        return Err("policy approval does not match observed transaction".into());
    }
    if approval.changes.len() > MAX_CHANGES {
        return Err("too many approved policy changes".into());
    }
    let current: Value =
        serde_json::from_slice(current_bytes).map_err(|error| error.to_string())?;
    let candidate: Value =
        serde_json::from_slice(candidate_bytes).map_err(|error| error.to_string())?;
    let current_bootstrap: BootstrapConfig =
        serde_json::from_value(current.clone()).map_err(|error| error.to_string())?;
    let candidate_bootstrap: BootstrapConfig =
        serde_json::from_value(candidate.clone()).map_err(|error| error.to_string())?;
    if current_bootstrap.development
        || current_bootstrap.allow_unsigned
        || current_bootstrap.trusted_keys.is_empty()
        || candidate_bootstrap.development
        || candidate_bootstrap.allow_unsigned
        || candidate_bootstrap.trusted_keys != current_bootstrap.trusted_keys
    {
        return Err("policy trial requires unchanged production trust".into());
    }
    let current_map = current.as_object().ok_or("invalid source bootstrap")?;
    let candidate_map = candidate.as_object().ok_or("invalid candidate bootstrap")?;
    for key in current_map.keys().chain(candidate_map.keys()) {
        if key != "policy" && current_map.get(key) != candidate_map.get(key) {
            return Err("policy approval cannot change bootstrap trust".into());
        }
    }
    let changes = policy_diff(&current, &candidate)?;
    if changes != approval.changes {
        return Err("candidate policy differs from the exact approved changes".into());
    }
    Ok(candidate_bootstrap)
}

/// Construct the exact trial policy from an independently provisioned host
/// allowlist. The host supplies the observed selection, signer and package
/// identity. A changed baseline is rejected before any policy is returned.
pub fn candidate_from_allowlist(
    allowlist: &PolicyAllowlist,
    context: &PolicyCandidateContext<'_>,
    current_bytes: &[u8],
) -> PackageResult<(Vec<u8>, PolicyApproval)> {
    if allowlist.publisher_fingerprint != context.publisher_fingerprint
        || allowlist.changes.len() > MAX_CHANGES
        || current_bytes.len() > MAX_BOOTSTRAP_BYTES
    {
        return Err("host policy allowlist does not match candidate".into());
    }
    let mut candidate: Value = serde_json::from_slice(current_bytes).map_err(|e| e.to_string())?;
    for (path, change) in &allowlist.changes {
        let (parent_path, encoded_key) = path.rsplit_once('/').ok_or("invalid policy path")?;
        if (parent_path != "/policy" && !parent_path.starts_with("/policy/"))
            || encoded_key.is_empty()
        {
            return Err("invalid policy allowlist path".into());
        }
        let mut key = String::new();
        let mut chars = encoded_key.chars();
        while let Some(c) = chars.next() {
            if c == '~' {
                match chars.next() {
                    Some('0') => key.push('~'),
                    Some('1') => key.push('/'),
                    _ => return Err("invalid policy path escape".into()),
                }
            } else {
                key.push(c);
            }
        }
        let parent = candidate
            .pointer_mut(parent_path)
            .and_then(Value::as_object_mut)
            .ok_or("policy allowlist parent is unavailable")?;
        let before = parent
            .get(&key)
            .cloned()
            .map_or(PolicyValue::Absent, PolicyValue::Present);
        // An optional composition may already be installed. Keep its exact
        // approved destination unchanged when updating the signed package.
        if before == change.after {
            continue;
        }
        if before != change.before {
            return Err("host policy changed since authorization".into());
        }
        match &change.after {
            PolicyValue::Absent => {
                parent.remove(&key);
            }
            PolicyValue::Present(value) => {
                parent.insert(key, value.clone());
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(&candidate).map_err(|e| e.to_string())?;
    let approval = propose_policy_approval(current_bytes, &bytes, context)?;
    verify_policy_candidate(allowlist, &approval, context, current_bytes, &bytes)?;
    Ok((bytes, approval))
}

/// Describe a candidate for offline review. This creates no authority: only an
/// independently protected host approval can authorize a production trial.
pub fn propose_policy_approval(
    current_bytes: &[u8],
    candidate_bytes: &[u8],
    context: &PolicyCandidateContext<'_>,
) -> PackageResult<PolicyApproval> {
    if current_bytes.len() > MAX_BOOTSTRAP_BYTES || candidate_bytes.len() > MAX_BOOTSTRAP_BYTES {
        return Err("bootstrap exceeds policy approval limit".into());
    }
    let current: Value =
        serde_json::from_slice(current_bytes).map_err(|error| error.to_string())?;
    let candidate: Value =
        serde_json::from_slice(candidate_bytes).map_err(|error| error.to_string())?;
    let approval = PolicyApproval {
        source_bootstrap_sha256: sha256(current_bytes),
        candidate_bootstrap_sha256: sha256(candidate_bytes),
        selection_revision: context.selection_revision.into(),
        publisher_fingerprint: context.publisher_fingerprint.into(),
        manifest_sha256: context.manifest_sha256.into(),
        changes: policy_diff(&current, &candidate)?,
    };
    verify_exact_policy_candidate(&approval, context, current_bytes, candidate_bytes)?;
    Ok(approval)
}

fn policy_diff(
    current: &Value,
    candidate: &Value,
) -> PackageResult<BTreeMap<String, ApprovedPolicyChange>> {
    let current_map = current.as_object().ok_or("invalid source bootstrap")?;
    let candidate_map = candidate.as_object().ok_or("invalid candidate bootstrap")?;
    let before = current_map
        .get("policy")
        .and_then(Value::as_object)
        .ok_or("source policy missing")?;
    let after = candidate_map
        .get("policy")
        .and_then(Value::as_object)
        .ok_or("candidate policy missing")?;
    let mut changes = BTreeMap::new();
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    for key in keys {
        if ![
            "principals",
            "settings",
            "processing_compositions",
            "api_methods",
            "legacy_native_contracts",
            "hooks",
            "service_layers",
        ]
        .contains(&key.as_str())
        {
            if before.get(key) != after.get(key) {
                return Err("policy approval cannot change host-only configuration".into());
            }
            continue;
        }
        collect_changes(
            &format!("/policy/{}", escape(key)),
            before.get(key),
            after.get(key),
            &mut changes,
        )?;
    }
    Ok(changes)
}

fn escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn collect_changes(
    path: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    changes: &mut BTreeMap<String, ApprovedPolicyChange>,
) -> PackageResult<()> {
    if before == after {
        return Ok(());
    }
    if let (Some(Value::Object(left)), Some(Value::Object(right))) = (before, after) {
        let keys: BTreeSet<_> = left.keys().chain(right.keys()).collect();
        for key in keys {
            collect_changes(
                &format!("{path}/{}", escape(key)),
                left.get(key),
                right.get(key),
                changes,
            )?;
        }
    } else {
        if changes.len() == MAX_CHANGES {
            return Err("too many policy changes".into());
        }
        let convert = |value: Option<&Value>| {
            value
                .cloned()
                .map_or(PolicyValue::Absent, PolicyValue::Present)
        };
        changes.insert(
            path.into(),
            ApprovedPolicyChange {
                before: convert(before),
                after: convert(after),
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn installed_policy_can_be_reused_but_drift_is_rejected() {
        let (_, installed, approval) = inputs();
        let (candidate, repeated) =
            candidate_from_allowlist(&allowlist(&approval), &context(&approval), &installed)
                .unwrap();
        assert!(repeated.changes.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&candidate).unwrap(),
            serde_json::from_slice::<Value>(&installed).unwrap()
        );
        let mut drift: Value = serde_json::from_slice(&installed).unwrap();
        drift["policy"]["settings"]["video"]["processor"] = json!("other/1");
        assert!(
            candidate_from_allowlist(
                &allowlist(&approval),
                &context(&approval),
                &serde_json::to_vec(&drift).unwrap()
            )
            .is_err()
        );
    }
    #[test]
    fn reconstructs_an_exact_top_level_api_method_approval() {
        let (source, _, observed) = inputs();
        let mut after: Value = serde_json::from_slice(&source).unwrap();
        after["policy"]["api_methods"] = json!([{"provider":"video","callers":["video"],"method":{"service":"example.processor/1","method":"status","interface_major":1,"interface_minor":0,"request":{"required":{}},"response":{"required":{}},"privilege":"user"}}]);
        let bytes = serde_json::to_vec(&after).unwrap();
        let approval = propose_policy_approval(&source, &bytes, &context(&observed)).unwrap();
        assert!(approval.changes.contains_key("/policy/api_methods"));
        let (candidate, _) =
            candidate_from_allowlist(&allowlist(&approval), &context(&approval), &source).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&candidate).unwrap(), after);
    }
    #[test]
    fn a_service_layer_change_is_an_approvable_policy_change() {
        let (source, _, observed) = inputs();
        let mut after: Value = serde_json::from_slice(&source).unwrap();
        after["policy"]["service_layers"] = json!({"example.echo/1": ["video"]});
        let bytes = serde_json::to_vec(&after).unwrap();
        let approval = propose_policy_approval(&source, &bytes, &context(&observed)).unwrap();
        assert!(
            approval
                .changes
                .keys()
                .any(|path| path.starts_with("/policy/service_layers")),
            "{:?}",
            approval.changes.keys()
        );
        let (candidate, _) =
            candidate_from_allowlist(&allowlist(&approval), &context(&approval), &source).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&candidate).unwrap(), after);
    }
    fn inputs() -> (Vec<u8>, Vec<u8>, PolicyApproval) {
        let before = json!({"development":false,"allow_unsigned":false,"trusted_keys":{"publisher":"key"},"policy":{"principals":{"video":{"native_use":[],"native_provide":[],"provide":[],"call":[],"publish":[],"subscribe":[],"hook":[]}},"settings":{"video":{"enabled":true}},"hooks":{}}});
        let mut after = before.clone();
        after["policy"]["settings"]["video"]["processor"] = json!("example.processor/1");
        let current = serde_json::to_vec(&before).unwrap();
        let candidate = serde_json::to_vec(&after).unwrap();
        let approval = PolicyApproval {
            source_bootstrap_sha256: sha256(&current),
            candidate_bootstrap_sha256: sha256(&candidate),
            selection_revision: "1".repeat(64),
            publisher_fingerprint: "2".repeat(64),
            manifest_sha256: "3".repeat(64),
            changes: BTreeMap::from([(
                "/policy/settings/video/processor".into(),
                ApprovedPolicyChange {
                    before: PolicyValue::Absent,
                    after: PolicyValue::Present(json!("example.processor/1")),
                },
            )]),
        };
        (current, candidate, approval)
    }
    fn context(approval: &PolicyApproval) -> PolicyCandidateContext<'_> {
        PolicyCandidateContext {
            selection_revision: &approval.selection_revision,
            publisher_fingerprint: &approval.publisher_fingerprint,
            manifest_sha256: &approval.manifest_sha256,
        }
    }
    fn allowlist(approval: &PolicyApproval) -> PolicyAllowlist {
        PolicyAllowlist {
            publisher_fingerprint: approval.publisher_fingerprint.clone(),
            changes: approval.changes.clone(),
        }
    }
    #[test]
    fn accepts_only_exact_approved_delta_and_observed_transaction() {
        let (source, candidate, approval) = inputs();
        assert!(
            verify_policy_candidate(
                &allowlist(&approval),
                &approval,
                &context(&approval),
                &source,
                &candidate
            )
            .is_ok()
        );
        let stale = PolicyCandidateContext {
            selection_revision: &"4".repeat(64),
            ..context(&approval)
        };
        assert!(
            verify_policy_candidate(
                &allowlist(&approval),
                &approval,
                &stale,
                &source,
                &candidate
            )
            .is_err()
        );
        let wrong_signer = PolicyCandidateContext {
            publisher_fingerprint: &"5".repeat(64),
            ..context(&approval)
        };
        assert!(
            verify_policy_candidate(
                &allowlist(&approval),
                &approval,
                &wrong_signer,
                &source,
                &candidate
            )
            .is_err()
        );
        let wrong_package = PolicyCandidateContext {
            manifest_sha256: &"6".repeat(64),
            ..context(&approval)
        };
        assert!(
            verify_policy_candidate(
                &allowlist(&approval),
                &approval,
                &wrong_package,
                &source,
                &candidate
            )
            .is_err()
        );
        let mut altered: Value = serde_json::from_slice(&candidate).unwrap();
        altered["policy"]["settings"]["video"]["other"] = json!(true);
        let altered = serde_json::to_vec(&altered).unwrap();
        // Even an exact self-generated proposal for the additional change
        // cannot expand the independently provisioned host allowlist.
        let self_approved =
            propose_policy_approval(&source, &altered, &context(&approval)).unwrap();
        assert!(
            verify_policy_candidate(
                &allowlist(&approval),
                &self_approved,
                &context(&approval),
                &source,
                &altered
            )
            .is_err()
        );
        let mut forged = approval.clone();
        forged.candidate_bootstrap_sha256 = sha256(&altered);
        assert!(
            verify_policy_candidate(
                &allowlist(&approval),
                &forged,
                &context(&forged),
                &source,
                &altered
            )
            .is_err()
        );
    }
    #[test]
    fn constructs_only_the_allowlisted_policy_for_the_observed_package() {
        let (source, expected, approval) = inputs();
        let authorized = allowlist(&approval);
        let (candidate, derived) =
            candidate_from_allowlist(&authorized, &context(&approval), &source).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&candidate).unwrap(),
            serde_json::from_slice::<Value>(&expected).unwrap()
        );
        assert_eq!(derived.changes, approval.changes);
        let mut stale: Value = serde_json::from_slice(&source).unwrap();
        stale["policy"]["settings"]["video"]["processor"] = json!("other");
        assert!(
            candidate_from_allowlist(
                &authorized,
                &context(&approval),
                &serde_json::to_vec(&stale).unwrap()
            )
            .is_err()
        );
        let wrong = PolicyCandidateContext {
            publisher_fingerprint: &"5".repeat(64),
            ..context(&approval)
        };
        assert!(candidate_from_allowlist(&authorized, &wrong, &source).is_err());
        let mut malicious = authorized.clone();
        malicious.changes.insert(
            "/trusted_keys/publisher".into(),
            ApprovedPolicyChange {
                before: PolicyValue::Present(json!("key")),
                after: PolicyValue::Present(json!("attacker")),
            },
        );
        assert!(candidate_from_allowlist(&malicious, &context(&approval), &source).is_err());
    }
    #[test]
    fn rejects_unapproved_trust_or_host_configuration_and_null_confusion() {
        let (source, candidate, approval) = inputs();
        for pointer in ["/trusted_keys/publisher", "/policy/host_lifecycle_control"] {
            let mut altered: Value = serde_json::from_slice(&candidate).unwrap();
            if pointer.starts_with("/trusted_keys") {
                altered["trusted_keys"]["publisher"] = json!("new");
            } else {
                altered["policy"]["host_lifecycle_control"] = json!(true);
            }
            let altered = serde_json::to_vec(&altered).unwrap();
            let mut forged = approval.clone();
            forged.candidate_bootstrap_sha256 = sha256(&altered);
            assert!(
                verify_policy_candidate(
                    &allowlist(&approval),
                    &forged,
                    &context(&forged),
                    &source,
                    &altered
                )
                .is_err()
            );
        }
        assert_ne!(PolicyValue::Absent, PolicyValue::Present(Value::Null));
    }
}
