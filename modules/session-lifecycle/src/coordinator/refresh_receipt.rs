//! Compact only proven terminal observation requests while preserving replay.
use super::*;
use sha2::{Digest, Sha256};

const OPERATION: &str = "refresh_provider_observations";

fn request_digest(request: &Value) -> Option<String> {
    let bytes = serde_json::to_vec(request).ok()?;
    Some(format!("{:x}", Sha256::digest(bytes)))
}

pub(super) fn is_compact(receipt: &Receipt) -> bool {
    let Some(object) = receipt.request.as_object() else {
        return false;
    };
    object.len() == 3
        && object.get("operation").and_then(Value::as_str) == Some(OPERATION)
        && object
            .get("request_sha256")
            .and_then(Value::as_str)
            .is_some()
}

pub(super) fn valid_compact(receipt: &Receipt) -> bool {
    if !is_compact(receipt) {
        return true;
    }
    receipt.work.is_none()
        && receipt.wait_target.is_none()
        && receipt.request["request_sha256"]
            .as_str()
            .is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        && receipt
            .response
            .get("target")
            .and_then(|target| serde_json::from_value::<LifecycleTarget>(target.clone()).ok())
            .is_some()
}

pub(super) fn matches(receipt: &Receipt, request: &Value) -> bool {
    if !is_compact(receipt) {
        return receipt.request == *request;
    }
    valid_compact(receipt)
        && request["operation"].as_str() == Some(OPERATION)
        && request_digest(request).as_deref() == receipt.request["request_sha256"].as_str()
}

/// Run on the private journal clone only after authority observations and their
/// independent metadata response have been staged together.
pub(super) fn compact(journal: &mut Journal, key: &str) -> bool {
    if journal.recovery_sources.contains_key(key)
        || journal
            .recovery_sources
            .values()
            .any(|source| source == key)
    {
        return false;
    }
    let Some(receipt) = journal.receipts.get(key) else {
        return false;
    };
    if receipt.work.is_some()
        || receipt.wait_target.is_some()
        || receipt.response["status"] != "succeeded"
        || receipt.request["operation"] != OPERATION
        || is_compact(receipt)
    {
        return false;
    }
    let Some(id) = receipt.request["operation_id"]
        .as_str()
        .filter(|id| valid_id(id))
    else {
        return false;
    };
    let Ok(target) = serde_json::from_value::<LifecycleTarget>(receipt.request["target"].clone())
    else {
        return false;
    };
    if receipt.response["target"] != receipt.request["target"]
        || receipt.response["operation_id"] != id
    {
        return false;
    }
    let Ok((epoch, session_id, _, _)) = super::operations::target_parts(&target) else {
        return false;
    };
    let Some(session) = journal
        .sessions
        .get(session_id)
        .filter(|session| session.host_epoch == epoch)
    else {
        return false;
    };
    let Ok(metadata) = super::provider_observations::metadata(journal, session, &receipt.caller)
    else {
        return false;
    };
    let Some(metadata) = metadata.as_object() else {
        return false;
    };
    let mut expected = json!({"status":"succeeded","operation_id":id,"target":target});
    expected.as_object_mut().unwrap().extend(metadata.clone());
    if receipt.response != expected {
        return false;
    }
    let Some(domains) = receipt.request["domains"].as_array() else {
        return false;
    };
    if domains.iter().any(|item| {
        let Some(domain) = item["domain"].as_str() else {
            return true;
        };
        let Some(state) = journal.authority.snapshot.domains.get(domain) else {
            return true;
        };
        item["revision"].as_u64() != Some(state.revision)
            || item["borrowers"] != json!(state.borrowers)
    }) {
        return false;
    }
    let resources = match receipt.request.get("resources") {
        Some(Value::Array(resources)) => resources.as_slice(),
        None => &[],
        _ => return false,
    };
    if resources.iter().any(|item| {
        let Ok(resource) = serde_json::from_value::<crate::planning::ResourceRef>(item.clone())
        else {
            return true;
        };
        journal
            .authority
            .snapshot
            .resources
            .get(&resource.provider)
            .and_then(|entries| entries.get(&resource.resource_id))
            != Some(&resource)
    }) {
        return false;
    }
    let Some(digest) = request_digest(&receipt.request) else {
        return false;
    };
    let compact = json!({"operation":OPERATION,"operation_id":id,"request_sha256":digest});
    if serde_json::to_vec(&compact)
        .ok()
        .zip(serde_json::to_vec(&receipt.request).ok())
        .is_none_or(|(a, b)| a.len() >= b.len())
    {
        return false;
    }
    journal.receipts.get_mut(key).unwrap().request = compact;
    true
}
