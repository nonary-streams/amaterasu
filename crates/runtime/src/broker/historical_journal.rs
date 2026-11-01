//! Validate retained State reads of pairing's canonical slot journal.
//! Journal data is neither a worker-retirement nor an external-cleanup receipt.
use amaterasu_contracts::{
    ApiMethodApproval, ApiPrivilege,
    effect_fence::{
        GuardedEffectIdentity, ProviderIncarnation, SelectedEffectMethod, SourceSlotLease,
    },
    valid_name,
};
use serde_json::Value;

fn token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn incarnation_valid(value: &ProviderIncarnation) -> bool {
    value.version == 1
        && value.id.len() == 32
        && value
            .id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn matches(
    request: &Value,
    response: &Value,
    issuer: &str,
    identity: &GuardedEffectIdentity,
    incarnation: &ProviderIncarnation,
    expected_entry: &Value,
) -> bool {
    let Some(slot) = identity.scope.source.slot() else {
        return false;
    };
    let key = format!("pairing/protocol-slot/v1/{slot:02}");
    let Some(journal) = response.get("value") else {
        return false;
    };
    let Some(claim) = journal.get("active").filter(|value| !value.is_null()) else {
        return false;
    };
    let Some(entries) = claim.get("entries").and_then(Value::as_array) else {
        return false;
    };
    let Some(retired) = claim.get("retired_through").and_then(Value::as_u64) else {
        return false;
    };
    let Some(issued) = claim.get("issued_through").and_then(Value::as_u64) else {
        return false;
    };
    let Some(slot_retired) = journal.get("retired_incarnation").and_then(Value::as_u64) else {
        return false;
    };
    let Some(lease) = claim
        .get("lease")
        .and_then(|value| serde_json::from_value::<SourceSlotLease>(value.clone()).ok())
    else {
        return false;
    };
    let Ok(scope) = serde_json::to_value(&identity.scope) else {
        return false;
    };
    if request.get("key").and_then(Value::as_str) != Some(key.as_str())
        || response.get("ok").and_then(Value::as_bool) != Some(true)
        || response.get("found").and_then(Value::as_bool) != Some(true)
        || response
            .get("revision")
            .and_then(Value::as_u64)
            .is_none_or(|n| n == 0)
        || journal.get("version").and_then(Value::as_u64) != Some(1)
        || journal.get("slot").and_then(Value::as_u64) != Some(slot.into())
        || journal
            .get("pending_close")
            .is_none_or(|value| !value.is_null())
        || serde_json::to_vec(journal).map_or(true, |bytes| bytes.len() > 12_000)
        || !incarnation_valid(incarnation)
        || lease.slot != slot
        || slot_retired.checked_add(1) != Some(lease.incarnation)
        || !token(&lease.broker_epoch)
        || !token(&lease.token)
        || claim.get("scope") != Some(&scope)
        || claim.get("issuer").and_then(Value::as_str) != Some(issuer)
        || entries.is_empty()
        || entries.len() > 8
        || issued.checked_sub(retired) != Some(entries.len() as u64)
        || identity.sequence <= retired
        || identity.sequence > issued
    {
        return false;
    }
    let Some(entry) = entries.get((identity.sequence - retired - 1) as usize) else {
        return false;
    };
    let Ok(identity_json) = serde_json::to_value(identity) else {
        return false;
    };
    let Ok(incarnation_json) = serde_json::to_value(incarnation) else {
        return false;
    };
    let Some(issued_lease) = entry
        .get("issued_lease")
        .and_then(|value| serde_json::from_value::<SourceSlotLease>(value.clone()).ok())
    else {
        return false;
    };
    let Some(selected) = entry
        .get("selected_method")
        .and_then(|value| serde_json::from_value::<SelectedEffectMethod>(value.clone()).ok())
    else {
        return false;
    };
    let cleanup = &entry["cleanup_method"];
    let Some(approval) = entry
        .get("cleanup_approval")
        .and_then(|value| serde_json::from_value::<ApiMethodApproval>(value.clone()).ok())
    else {
        return false;
    };
    let Some(cleanup_service) = cleanup
        .get("service")
        .and_then(Value::as_str)
        .filter(|name| valid_name(name))
    else {
        return false;
    };
    let Some(cleanup_method) = cleanup
        .get("method")
        .and_then(Value::as_str)
        .filter(|name| valid_name(name))
    else {
        return false;
    };
    let Some(cleanup_major) = cleanup
        .get("interface_major")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
    else {
        return false;
    };
    let Some(cleanup_minor) = cleanup.get("interface_minor").and_then(Value::as_u64) else {
        return false;
    };
    let mut saw_unconfirmed = false;
    let valid_window =
        entries.iter().enumerate().all(|(index, item)| {
            let Some(item_identity) = item.get("identity").and_then(|value| {
                serde_json::from_value::<GuardedEffectIdentity>(value.clone()).ok()
            }) else {
                return false;
            };
            let Some(item_instance) = item.get("provider_incarnation").and_then(|value| {
                serde_json::from_value::<ProviderIncarnation>(value.clone()).ok()
            }) else {
                return false;
            };
            let Some(item_lease) = item
                .get("issued_lease")
                .and_then(|value| serde_json::from_value::<SourceSlotLease>(value.clone()).ok())
            else {
                return false;
            };
            let Some(item_selection) = item.get("selected_method").and_then(|value| {
                serde_json::from_value::<SelectedEffectMethod>(value.clone()).ok()
            }) else {
                return false;
            };
            let Some(cleaned) = item.get("cleanup_confirmed").and_then(Value::as_bool) else {
                return false;
            };
            if !cleaned {
                saw_unconfirmed = true;
            }
            !(cleaned && saw_unconfirmed)
                && retired.checked_add(index as u64 + 1) == Some(item_identity.sequence)
                && item_identity.scope == identity.scope
                && incarnation_valid(&item_instance)
                && item_lease.slot == slot
                && item_lease.incarnation == lease.incarnation
                && token(&item_lease.broker_epoch)
                && token(&item_lease.token)
                && item_selection.version == 1
                && item_selection.provider == item_identity.provider
                && item_selection.provider_incarnation.as_ref() == Some(&item_instance)
                && item_selection.method.service == item_identity.service
                && item_selection.method.method == item_identity.method
                && item_selection.method.interface_major == item_identity.interface_major
                && item_selection.method.interface_minor == item_identity.interface_minor
                && item_selection.method.privilege == ApiPrivilege::User
        });
    entry == expected_entry
        && valid_window
        && entry.get("identity") == Some(&identity_json)
        && entry.get("provider_incarnation") == Some(&incarnation_json)
        && selected.provider_incarnation.as_ref() == Some(incarnation)
        && issued_lease.slot == slot
        && issued_lease.incarnation == lease.incarnation
        && token(&issued_lease.broker_epoch)
        && token(&issued_lease.token)
        && selected.version == 1
        && selected.provider == identity.provider
        && selected.method.service == identity.service
        && selected.method.method == identity.method
        && selected.method.interface_major == identity.interface_major
        && selected.method.interface_minor == identity.interface_minor
        && selected.method.privilege == ApiPrivilege::User
        && entry
            .get("cleanup_confirmed")
            .and_then(Value::as_bool)
            .is_some()
        && entry
            .pointer("/cleanup/id")
            .and_then(Value::as_str)
            .is_some_and(|id| {
                id.len() == 64
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        && approval.provider == identity.provider
        && approval.method.privilege == ApiPrivilege::User
        && approval.method.service == cleanup_service
        && approval.method.method == cleanup_method
        && u64::from(approval.method.interface_major) == cleanup_major
        && u64::from(approval.method.interface_minor) == cleanup_minor
        && (cleanup_service != identity.service
            || cleanup_method != identity.method
            || cleanup_major != u64::from(identity.interface_major))
        && cleanup
            .get("schema_revision")
            .and_then(Value::as_u64)
            .is_some_and(|value| value != 0)
        && approval.callers.contains(issuer)
}
