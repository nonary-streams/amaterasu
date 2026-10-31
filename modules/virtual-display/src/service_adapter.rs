//! Bridge the approved display service to the existing caller-owned manager.
//! No operation is inferred from a service grant: unsupported manager features
//! are rejected before any native effect.
use crate::model::Operation;
use amaterasu_module_sdk::virtual_display_service::{
    Capabilities, EffectState, Layout, ModeObservation, OwnerReference, Provider, Reply, Request,
    TopologyRevision,
};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_OPERATIONS: usize = 128;

pub trait Backend {
    type Admission;
    fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Self::Admission>);
    fn completion_lost(&mut self, admission: Option<Self::Admission>);
}

#[cfg(any(not(amaterasu_libvirtualdisplay), test))]
impl<D: crate::backend::Driver> Backend for crate::manager::Manager<D> {
    type Admission = crate::manager::Admission;
    fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Self::Admission>) {
        self.admit(caller, operation)
    }
    fn completion_lost(&mut self, admission: Option<Self::Admission>) {
        self.completion_lost(admission);
    }
}
#[cfg(amaterasu_libvirtualdisplay)]
impl Backend for crate::linux_manager::Manager {
    type Admission = crate::linux_manager::Admission;
    fn admit(&mut self, caller: &str, operation: Operation) -> (Value, Option<Self::Admission>) {
        self.admit(caller, operation)
    }
    fn completion_lost(&mut self, admission: Option<Self::Admission>) {
        self.completion_lost(admission);
    }
}

#[derive(Default)]
pub struct Adapter {
    operations: BTreeMap<(String, String), (Request, Reply)>,
}

pub struct Invocation<'a, B: Backend> {
    adapter: &'a mut Adapter,
    backend: &'a mut B,
    admission: Option<B::Admission>,
}
impl Adapter {
    pub fn invocation<'a, B: Backend>(&'a mut self, backend: &'a mut B) -> Invocation<'a, B> {
        Invocation {
            adapter: self,
            backend,
            admission: None,
        }
    }
}
impl<B: Backend> Invocation<'_, B> {
    fn status(&mut self, caller: &str, owner: Option<&OwnerReference>) -> Value {
        self.backend
            .admit(
                caller,
                Operation::Status {
                    identity: owner.map(|o| o.identity.clone()),
                    reference_id: owner.map(|o| o.reference_id.clone()),
                },
            )
            .0
    }
    fn topology(status: &Value) -> Option<TopologyRevision> {
        let domain = status.get("mutation_domain")?;
        Some(TopologyRevision {
            domain_id: domain.get("id")?.as_str()?.into(),
            revision: domain.get("revision")?.as_u64()?,
        })
    }
    fn inspect(&mut self, caller: &str, owner: &OwnerReference) -> Reply {
        let status = self.status(caller, Some(owner));
        if status["ok"] != true {
            return failure(&status);
        }
        if status["reference_owned"] != true {
            return Reply::NotOwned;
        }
        let state = match status["monitors"][0]["state"].as_str() {
            Some("cleanup_failed" | "ownership_conflict" | "reclaim_pending") => {
                EffectState::RecoveryRequired
            }
            // Ready reports output publication, never verified mode or HDR.
            Some("ready") => EffectState::Applied,
            _ => EffectState::Pending,
        };
        Reply::Effect {
            state,
            topology: Self::topology(&status),
            observation: ModeObservation::NotObserved,
        }
    }
    fn mutate(
        &mut self,
        caller: &str,
        request: &Request,
        owner: &OwnerReference,
        operation_id: &str,
        expected: &TopologyRevision,
    ) -> Reply {
        let key = (caller.to_owned(), operation_id.to_owned());
        if let Some((old, reply)) = self.adapter.operations.get(&key) {
            return if old == request {
                reply.clone()
            } else {
                Reply::Conflict
            };
        }
        if self.adapter.operations.len() >= MAX_OPERATIONS {
            return Reply::ResourceExhausted;
        }
        let status = self.status(caller, None);
        let Some(current) = Self::topology(&status) else {
            return Reply::Unavailable;
        };
        if &current != expected {
            return Reply::StaleTopology { current };
        }
        let operation = match request {
            Request::Create {
                mode,
                name,
                layout,
                scale,
                ..
            } => {
                if status["enabled"] != true || status["recovery_ready"] != true {
                    return Reply::Unavailable;
                }
                let layout = match layout {
                    Layout::Exclusive => {
                        amaterasu_module_sdk::virtual_display::VirtualDisplayLayout::Exclusive
                    }
                    Layout::Extended => {
                        amaterasu_module_sdk::virtual_display::VirtualDisplayLayout::Extended
                    }
                    _ => return Reply::Unavailable,
                };
                Operation::Create {
                    hdr_enabled: mode.hdr,
                    identity: owner.identity.clone(),
                    width: mode.width,
                    height: mode.height,
                    refresh_millihz: mode.refresh_millihz,
                    name: name.clone(),
                    virtual_display_layout: Some(layout),
                    expected_topology_revision: Some(expected.revision),
                    expected_topology_domain_id: Some(expected.domain_id.clone()),
                    reference_id: Some(owner.reference_id.clone()),
                    dd_virtual_display_scale: *scale,
                }
            }
            Request::Remove { .. } => {
                match self.inspect(caller, owner) {
                    Reply::Effect { .. } => {}
                    other => return other,
                }
                Operation::Remove {
                    identity: owner.identity.clone(),
                    reference_id: Some(owner.reference_id.clone()),
                }
            }
            _ => return Reply::Unavailable,
        };
        let (result, admission) = self.backend.admit(caller, operation);
        self.admission = admission;
        if result["ok"] != true {
            return failure(&result);
        }
        let reply = Reply::Effect {
            state: if result["removed"] == true {
                EffectState::Removed
            } else {
                EffectState::Pending
            },
            topology: Self::topology(&self.status(caller, None)),
            observation: ModeObservation::NotObserved,
        };
        self.adapter
            .operations
            .insert(key, (request.clone(), reply.clone()));
        reply
    }
}
fn failure(value: &Value) -> Reply {
    match value["code"].as_str() {
        Some("invalid_request") => Reply::InvalidRequest,
        Some("not_found" | "reference_not_owned" | "borrow_invalid") => Reply::NotOwned,
        Some("resource_exhausted" | "monitor_limit" | "reference_limit" | "busy") => {
            Reply::ResourceExhausted
        }
        Some("identity_conflict" | "reference_conflict" | "ownership_conflict") => Reply::Conflict,
        _ => Reply::Unavailable,
    }
}
impl<B: Backend> Provider for Invocation<'_, B> {
    fn execute(&mut self, caller: &str, request: &Request) -> Reply {
        match request {
            Request::Capabilities => {
                let status = self.status(caller, None);
                let topology = Self::topology(&status);
                let create = topology.is_some()
                    && status["enabled"] == true
                    && status["recovery_ready"] == true;
                Reply::Capabilities {
                    capabilities: Capabilities {
                        create,
                        reconfigure: false,
                        borrow: false,
                        recovery: true,
                        topology,
                        layouts: if create {
                            vec![Layout::Exclusive, Layout::Extended]
                        } else {
                            vec![]
                        },
                    },
                }
            }
            Request::Inspect { owner } => self.inspect(caller, owner),
            Request::Create {
                owner,
                operation_id,
                expected,
                ..
            }
            | Request::Remove {
                owner,
                operation_id,
                expected,
            } => self.mutate(caller, request, owner, operation_id, expected),
            Request::Recover {
                owner,
                operation_id,
            } => {
                let record = self
                    .adapter
                    .operations
                    .get(&(caller.into(), operation_id.clone()))
                    .cloned();
                match record {
                    Some((
                        Request::Create {
                            owner: original, ..
                        },
                        Reply::Effect {
                            state: EffectState::Pending,
                            ..
                        },
                    )) if original == *owner => self.inspect(caller, owner),
                    Some((
                        Request::Create {
                            owner: original, ..
                        }
                        | Request::Remove {
                            owner: original, ..
                        },
                        reply,
                    )) if original == *owner => reply,
                    _ => Reply::Unavailable,
                }
            }
            Request::ReleaseOperation {
                owner,
                operation_id,
            } => {
                let key = (caller.to_owned(), operation_id.clone());
                let owned = matches!(self.adapter.operations.get(&key),
                    Some((Request::Create { owner: original, .. } | Request::Remove { owner: original, .. }, _)) if original == owner);
                if owned {
                    self.adapter.operations.remove(&key);
                    Reply::OperationReleased
                } else {
                    Reply::NotOwned
                }
            }
            _ => Reply::Unavailable,
        }
    }
    fn completion_lost(&mut self, caller: &str, request: &Request, _: &Reply) {
        self.backend.completion_lost(self.admission.take());
        if let Request::Create { operation_id, .. } | Request::Remove { operation_id, .. } = request
            && let Some((_, reply)) = self
                .adapter
                .operations
                .get_mut(&(caller.into(), operation_id.clone()))
        {
            *reply = Reply::Effect {
                state: EffectState::RecoveryRequired,
                topology: None,
                observation: ModeObservation::NotObserved,
            };
        }
    }
    fn shutdown(&mut self) { /* The owning module drops the manager, which retires its workers. */
    }
}
