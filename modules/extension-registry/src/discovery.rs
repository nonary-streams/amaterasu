//! Read-only metadata filtered by installed operator policy and the broker's delegated approvals.
use crate::policy::{Settings, approved};
use amaterasu_contracts::ApiMethodApproval;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};

const PAGE_BYTES: usize = 11_500;
const MAX_APPROVALS: usize = 4096;

pub fn approvals(
    runtime: &RuntimeClient,
    parent: u64,
) -> ModuleResult<(u64, Vec<ApiMethodApproval>)> {
    approvals_with(parent, |command| runtime.send(command))
}

fn approvals_with(
    parent: u64,
    mut send: impl FnMut(Command) -> ModuleResult<Response>,
) -> ModuleResult<(u64, Vec<ApiMethodApproval>)> {
    let mut result = Vec::new();
    let mut after = None;
    let mut snapshot_revision = None;
    loop {
        match send(Command::ListApiApprovals {
            parent: Some(parent),
            after,
            limit: 16,
        })? {
            Response::ApiApprovals {
                revision,
                approvals,
                next,
                ..
            } => {
                if snapshot_revision.is_some_and(|previous| previous != revision) {
                    return Err("approval revision changed".into());
                }
                snapshot_revision = Some(revision);
                result.extend(approvals);
                if result.len() > MAX_APPROVALS {
                    return Err("too many approved API methods".into());
                }
                if next.is_none() {
                    return Ok((revision, result));
                }
                after = next;
            }
            Response::Error { code, message } => return Err(format!("{code}: {message}")),
            _ => return Err("invalid API approval response".into()),
        }
    }
}

pub fn list(
    settings: &Settings,
    caller: &str,
    initiator: &str,
    revision: u64,
    approvals: &[ApiMethodApproval],
    after: Option<&str>,
    limit: usize,
) -> Value {
    if limit == 0 || limit > 128 {
        return failure("invalid_request", "invalid page limit");
    }
    let mut items = Vec::new();
    let mut eligible = settings
        .descriptors
        .iter()
        .filter(|item| {
            item.grants
                .iter()
                .any(|grant| grant.caller == caller && grant.read)
                && item
                    .grants
                    .iter()
                    .any(|grant| grant.caller == initiator && grant.read)
                && approved(item, &item.descriptor.settings_validate, approvals)
                && after.is_none_or(|after| item.descriptor.provider.as_str() > after)
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| left.descriptor.provider.cmp(&right.descriptor.provider));
    let mut more = false;
    for item in eligible {
        if items.len() == limit {
            more = true;
            break;
        }
        let mut descriptor = item.descriptor.clone();
        descriptor.actions.retain(|action| {
            approved(item, &action.execute, approvals)
                && action
                    .plan
                    .as_ref()
                    .is_none_or(|method| approved(item, method, approvals))
                && action
                    .status
                    .as_ref()
                    .is_none_or(|method| approved(item, method, approvals))
        });
        items.push(descriptor);
        if serde_json::to_vec(&items).map_or(true, |bytes| bytes.len() > PAGE_BYTES) {
            items.pop();
            if items.is_empty() {
                return failure("resource_exhausted", "descriptor exceeds page budget");
            }
            more = true;
            break;
        }
    }
    let next = if more {
        items.last().map(|item| item.provider.clone())
    } else {
        None
    };
    json!({"ok":true,"status":"ok","revision":revision,"items":items,"next":next})
}

pub fn describe(
    settings: &Settings,
    caller: &str,
    initiator: &str,
    provider: &str,
    approvals: &[ApiMethodApproval],
) -> Option<amaterasu_extension_api::ExtensionDescriptor> {
    let item = settings.descriptors.iter().find(|item| {
        item.descriptor.provider == provider
            && item
                .grants
                .iter()
                .any(|grant| grant.caller == caller && grant.read)
            && item
                .grants
                .iter()
                .any(|grant| grant.caller == initiator && grant.read)
            && approved(item, &item.descriptor.settings_validate, approvals)
    })?;
    let mut descriptor = item.descriptor.clone();
    descriptor.actions.retain(|action| {
        approved(item, &action.execute, approvals)
            && action
                .plan
                .as_ref()
                .is_none_or(|method| approved(item, method, approvals))
            && action
                .status
                .as_ref()
                .is_none_or(|method| approved(item, method, approvals))
    });
    Some(descriptor)
}

pub fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"status":"error","error":{"code":code,"message":message}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::ApiApprovalCursor;

    #[test]
    fn discovery_obeys_broker_page_limit_and_preserves_snapshot_revision() {
        let mut calls = 0;
        let (revision, approvals) = approvals_with(42, |command| {
            let Command::ListApiApprovals {
                parent,
                after,
                limit,
            } = command
            else {
                panic!("wrong broker command")
            };
            assert_eq!(parent, Some(42));
            assert!((1..=16).contains(&limit));
            calls += 1;
            let next = if after.is_none() {
                Some(ApiApprovalCursor {
                    revision: 7,
                    service: "svc".into(),
                    method: "method".into(),
                    interface_major: 1,
                })
            } else {
                None
            };
            Ok(Response::ApiApprovals {
                revision: 7,
                approvals: vec![],
                next,
            })
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(revision, 7);
        assert!(approvals.is_empty());
        let mut calls = 0;
        assert!(
            approvals_with(42, |_| {
                calls += 1;
                Ok(Response::ApiApprovals {
                    revision: calls,
                    approvals: vec![],
                    next: if calls == 1 {
                        Some(ApiApprovalCursor {
                            revision: 1,
                            service: "svc".into(),
                            method: "m".into(),
                            interface_major: 1,
                        })
                    } else {
                        None
                    },
                })
            })
            .is_err()
        );
    }
}
