//! Broker dispatch with separate read and execution-resolution permissions.
//! The library implementation owns storage, policy, revisions and scan semantics.
use super::{Capabilities, Entry, MAX_RESPONSE_BYTES, Page, Query, Request, key};
use crate::requests::IncomingRequest;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub use crate::application_library::provider_adapter::Identity;

pub trait Library {
    fn capabilities(&self) -> Capabilities;
    fn list(
        &mut self,
        identity: &Identity<'_>,
        query: &Query,
        cursor: Option<&super::Cursor>,
        limit: usize,
    ) -> Result<Page, &'static str>;
    fn resolve(&mut self, identity: &Identity<'_>, id: &str) -> Result<Entry, &'static str>;
    /// Recheck visibility, readiness and the exact entry revision. Return an
    /// opaque executor reference, never commands or caller-selected routes.
    fn resolve_launch(
        &mut self,
        _identity: &Identity<'_>,
        _id: &str,
        _revision: &str,
    ) -> Result<super::LaunchTarget, &'static str> {
        Err("unsupported_capability")
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CallerPolicy {
    pub initiators: BTreeSet<String>,
    pub read: bool,
    pub resolve_launch: bool,
}

pub struct LibraryAdapter<L> {
    library: L,
    service: String,
    callers: BTreeMap<String, CallerPolicy>,
}

impl<L: Library> LibraryAdapter<L> {
    pub fn new(
        library: L,
        service: String,
        callers: BTreeMap<String, CallerPolicy>,
    ) -> Result<Self, &'static str> {
        library.capabilities().validate()?;
        if service.strip_suffix("/2").is_none_or(|name| !key(name))
            || callers.len() > 32
            || callers.iter().any(|(caller, policy)| {
                !key(caller)
                    || policy.initiators.len() > 32
                    || policy.initiators.iter().any(|id| !key(id))
            })
        {
            return Err("invalid_library_policy");
        }
        Ok(Self {
            library,
            service,
            callers,
        })
    }

    pub fn dispatch(&mut self, incoming: &IncomingRequest<Value>) -> Value {
        let Some(policy) = self.callers.get(&incoming.caller) else {
            return failure("unauthorized");
        };
        if incoming.service != self.service || !policy.initiators.contains(&incoming.initiator) {
            return failure("unauthorized");
        }
        if incoming.remaining_ms == 0 {
            return failure("deadline_expired");
        }
        if !serde_json::to_vec(&incoming.payload)
            .is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES)
        {
            return failure("invalid_request");
        }
        let Ok(request) = incoming.decode::<Request>() else {
            return failure("invalid_request");
        };
        if let Err(code) = request.validate() {
            return failure(code);
        }
        let permitted = match request {
            Request::ResolveLaunch { .. } => policy.resolve_launch,
            Request::Describe {} => policy.read || policy.resolve_launch,
            _ => policy.read,
        };
        if !permitted {
            return failure("unauthorized");
        }
        let identity = Identity {
            caller: &incoming.caller,
            initiator: &incoming.initiator,
        };
        let result = self.execute(&identity, request);
        match result {
            Ok(value)
                if serde_json::to_vec(&value)
                    .is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES) =>
            {
                value
            }
            Ok(_) => failure("response_too_large"),
            Err(code) => failure(code),
        }
    }

    fn execute(
        &mut self,
        identity: &Identity<'_>,
        request: Request,
    ) -> Result<Value, &'static str> {
        let capabilities = self.library.capabilities();
        capabilities.validate()?;
        match request {
            Request::Describe {} => Ok(json!({"ok":true,"capabilities":capabilities})),
            Request::List {
                query,
                cursor,
                limit,
            } => {
                if !query.text.is_empty() && !capabilities.supports(super::SEARCH) {
                    return Err("unsupported_capability");
                }
                let page = self
                    .library
                    .list(identity, &query, cursor.as_ref(), limit)?;
                page.validate_for(cursor.as_ref(), limit)?;
                Ok(json!({"ok":true,"page":page}))
            }
            Request::Resolve { id } => {
                let entry = self.library.resolve(identity, &id)?;
                entry.validate()?;
                if entry.id != id {
                    return Err("invalid_library_response");
                }
                Ok(json!({"ok":true,"entry":entry}))
            }
            Request::ResolveLaunch { id, revision } => {
                if !capabilities.supports(super::RESOLVE_LAUNCH) {
                    return Err("unsupported_capability");
                }
                let target = self.library.resolve_launch(identity, &id, &revision)?;
                target.validate_for(&id, &revision)?;
                Ok(json!({"ok":true,"target":target}))
            }
        }
    }
}

fn failure(code: &str) -> Value {
    let code = if key(code) { code } else { "library_error" };
    json!({"ok":false,"error":{"code":code}})
}
