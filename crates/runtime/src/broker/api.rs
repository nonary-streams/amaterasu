//! Installed method contracts for dynamically named user-process services.
//! Approval is policy data, never self-issued by a native publisher.
use amaterasu_contracts::{
    ApiApprovalCursor, ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Grants,
    Response, RuntimeConfig,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

use super::errors::permission_denied;

type MethodKey = (String, String, u32);

pub(super) struct ApiInvocation<'a> {
    pub service: &'a str,
    pub method: &'a str,
    pub major: u32,
    pub minor: u32,
    pub payload: &'a Value,
}

pub(super) struct ApiQuery<'a> {
    pub service: &'a str,
    pub method: &'a str,
    pub major: u32,
    pub min_minor: u32,
}

pub(super) struct ApiRegistry {
    approved: BTreeMap<MethodKey, ApiMethodApproval>,
    registered: BTreeSet<MethodKey>,
    revision: u64,
}

impl ApiRegistry {
    pub fn new(configuration: &RuntimeConfig) -> Self {
        Self {
            approved: configuration
                .api_methods
                .iter()
                .cloned()
                .map(|approval| (key(&approval.method), approval))
                .collect(),
            registered: BTreeSet::new(),
            revision: 0,
        }
    }

    pub fn owns_service(&self, service: &str) -> bool {
        self.approved.keys().any(|key| key.0 == service)
    }

    pub fn register(&mut self, caller: &str, grants: &Grants, method: ApiMethod) -> Response {
        let key = key(&method);
        let Some(approval) = self.approved.get(&key) else {
            return permission_denied();
        };
        if approval.provider != caller
            || approval.method != method
            || approval.method.privilege != ApiPrivilege::User
            || !grants.provide.contains(&method.service)
        {
            return permission_denied();
        }
        if self.registered.insert(key) {
            self.revision += 1;
        }
        Response::Ok
    }

    pub fn query(
        &self,
        caller: &str,
        grants: &Grants,
        query: ApiQuery<'_>,
        include_approval: bool,
    ) -> Response {
        let Some(approval) =
            self.authorized(caller, grants, query.service, query.method, query.major)
        else {
            return permission_denied();
        };
        if approval.method.interface_minor < query.min_minor {
            return Response::error("incompatible_api", "interface minor version is too old");
        }
        let payload = if include_approval {
            serde_json::to_value(approval)
        } else {
            serde_json::to_value(&approval.method)
        };
        Response::Result {
            payload: payload.expect("API approval is serializable"),
        }
    }

    /// Enumerate only methods this authenticated endpoint can query and call.
    pub fn list_approvals(
        &self,
        caller: &str,
        grants: &Grants,
        authority: Option<&(String, BTreeSet<String>)>,
        after: Option<ApiApprovalCursor>,
        limit: u16,
    ) -> Response {
        if !(1..=16).contains(&limit) {
            return Response::error("invalid_request", "page limit must be 1 through 16");
        }
        if after
            .as_ref()
            .is_some_and(|cursor| cursor.revision != self.revision)
        {
            return Response::error(
                "stale_revision",
                "API registrations changed during discovery",
            );
        }
        let after_key = after.map(|cursor| (cursor.service, cursor.method, cursor.interface_major));
        let mut eligible = self
            .approved
            .iter()
            .filter(|(key, _)| {
                after_key.as_ref().is_none_or(|after| *key > after)
                    && authority.is_none_or(|(initiator, allowed)| {
                        allowed.contains(&key.0) && self.approved[*key].callers.contains(initiator)
                    })
                    && self
                        .authorized(caller, grants, &key.0, &key.1, key.2)
                        .is_some()
            })
            .peekable();
        let mut approvals = Vec::new();
        let mut last = None;
        while let Some((key, approval)) = eligible.peek().copied() {
            if approvals.len() == usize::from(limit) {
                break;
            }
            approvals.push(approval.clone());
            // Bound serialized output, including schemas controlled by installed policy.
            if serde_json::to_vec(&approvals).map_or(true, |bytes| bytes.len() > 11_500) {
                approvals.pop();
                if approvals.is_empty() {
                    return Response::error(
                        "resource_exhausted",
                        "approval exceeds discovery page limit",
                    );
                }
                break;
            }
            last = Some(ApiApprovalCursor {
                revision: self.revision,
                service: key.0.clone(),
                method: key.1.clone(),
                interface_major: key.2,
            });
            eligible.next();
        }
        Response::ApiApprovals {
            revision: self.revision,
            approvals,
            next: if eligible.peek().is_some() {
                last
            } else {
                None
            },
        }
    }

    pub fn callable(
        &self,
        caller: &str,
        initiator: &str,
        grants: &Grants,
        invocation: ApiInvocation<'_>,
    ) -> Result<(Value, ApiSchema), Response> {
        let approval = self
            .authorized(
                caller,
                grants,
                invocation.service,
                invocation.method,
                invocation.major,
            )
            .ok_or_else(permission_denied)?;
        if !approval.callers.contains(initiator) {
            return Err(permission_denied());
        }
        if approval.method.interface_minor != invocation.minor {
            return Err(Response::error(
                "incompatible_api",
                "interface minor version differs",
            ));
        }
        if !payload_matches(&approval.method.request, invocation.payload) {
            return Err(Response::error(
                "invalid_request",
                "API request does not match its installed schema",
            ));
        }
        Ok((
            json!({"method":invocation.method,"interface_major":invocation.major,"interface_minor":invocation.minor,"arguments":invocation.payload}),
            approval.method.response.clone(),
        ))
    }

    fn authorized(
        &self,
        caller: &str,
        grants: &Grants,
        service: &str,
        method: &str,
        major: u32,
    ) -> Option<&ApiMethodApproval> {
        let key = (service.into(), method.into(), major);
        let approval = self.approved.get(&key)?;
        (self.registered.contains(&key)
            && approval.method.privilege == ApiPrivilege::User
            && approval.callers.contains(caller)
            && grants.call.contains(service))
        .then_some(approval)
    }
}

fn key(method: &ApiMethod) -> MethodKey {
    (
        method.service.clone(),
        method.method.clone(),
        method.interface_major,
    )
}

pub(super) fn validate_approvals(configuration: &RuntimeConfig) -> Result<(), String> {
    amaterasu_contracts::api_validation::validate_approvals(configuration)
}

pub(super) fn payload_matches(schema: &ApiSchema, payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    schema.required.iter().all(|(name, kind)| {
        object
            .get(name)
            .is_some_and(|value| kind_matches(kind, value))
    }) && object.iter().all(|(name, value)| {
        schema
            .required
            .get(name)
            .or_else(|| schema.optional.get(name))
            .map_or(schema.allow_extra, |kind| kind_matches(kind, value))
    })
}

fn kind_matches(kind: &ApiFieldKind, value: &Value) -> bool {
    match kind {
        ApiFieldKind::Any => true,
        ApiFieldKind::Null => value.is_null(),
        ApiFieldKind::Boolean => value.is_boolean(),
        ApiFieldKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        ApiFieldKind::Number => value.is_number(),
        ApiFieldKind::String => value.is_string(),
        ApiFieldKind::Array => value.is_array(),
        ApiFieldKind::Object => value.is_object(),
    }
}
