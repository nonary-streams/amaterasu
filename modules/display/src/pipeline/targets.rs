//! Journaled references to existing outputs, without native creation or removal.
use super::*;
use crate::scoped::{self, ScopedReservation};
use crate::targets;
use amaterasu_module_sdk::display::targets::Binding;

impl<P: Platform> Pipeline<P> {
    pub(super) fn start_targets(
        &mut self,
        incoming: Incoming,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let result=self.platform.capture_targets().and_then(|targets|{
            targets::validate_inventory(&targets)?;
            match incoming.operation {
                Operation::CaptureTargets{}=>Ok(json!({"ok":true,"targets":targets.into_iter().map(|target|target.descriptor).collect::<Vec<_>>() })),
                Operation::ResolveCaptureTarget{selector}=>targets::select(targets,&selector)?.resolve_reply(),
                _=>unreachable!("read-only target operations"),
            }
        });
        complete(
            send,
            incoming.context.request,
            result.unwrap_or_else(DisplayError::payload),
        )
    }

    pub(super) fn loaded_targets(
        &mut self,
        context: Context,
        operation: Operation,
        revision: u64,
        journal: Option<Journal>,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if journal.is_none()
            && let Operation::ReleaseTarget {
                operation_id,
                selector,
                reference_id,
            } = &operation
        {
            for value in [operation_id, selector, reference_id] {
                if let Err(error) = scoped::validate_identity(value) {
                    return complete(send, context.request, error.payload());
                }
            }
            return complete(
                send,
                context.request,
                json!({"ok":true,"released":true,"native_removed":false,"selector":selector,"reference_id":reference_id}),
            );
        }
        match self.prepare_target_reference(&context, operation, revision, journal) {
            Ok((journal, reply, selector)) => {
                if let Err(error) = check_request(send, &context) {
                    return complete(send, context.request, error.payload());
                }
                if !journal.normal_active && journal.scoped_targets.is_empty() {
                    return self.clear(context, revision, reply, send);
                }
                if let Err(error) = journal.validate() {
                    return complete(send, context.request, error.payload());
                }
                let stage = match selector {
                    Some(selector) => Stage::TargetsSaved {
                        reply,
                        selector,
                        active_lease: journal.normal_active.then(|| journal.lease.clone()),
                    },
                    None => Stage::Saved {
                        reply,
                        active_lease: journal.normal_active.then(|| journal.lease.clone()),
                    },
                };
                self.submit(context,json!({"action":"set","key":JOURNAL_KEY,"expected_revision":revision,"value":journal}),stage,send)
            }
            Err(error) => complete(send, context.request, error.payload()),
        }
    }

    fn prepare_target_reference(
        &mut self,
        context: &Context,
        operation: Operation,
        revision: u64,
        journal: Option<Journal>,
    ) -> DisplayResult<(Journal, Value, Option<String>)> {
        let request_identity = serde_json::to_value(&operation)
            .map_err(|error| DisplayError::new("invalid_request", error.to_string()))?;
        let (operation_id, selector, reference_id, releasing) = match operation {
            Operation::BorrowTarget {
                operation_id,
                selector,
                reference_id,
            } => (operation_id, selector, reference_id, false),
            Operation::ReleaseTarget {
                operation_id,
                selector,
                reference_id,
            } => (operation_id, selector, reference_id, true),
            _ => unreachable!("target references only"),
        };
        for value in [&operation_id, &reference_id] {
            scoped::validate_identity(value)?;
        }
        if selector.len() > 128 {
            return Err(DisplayError::new(
                "invalid_request",
                "target selector is too long",
            ));
        }
        scoped::validate_identity(&selector)?;
        let mut journal = match journal {
            Some(journal) => journal,
            None => Journal {
                normal_active: false,
                scoped_targets: Vec::new(),
                scoped_pending: None,
                scoped_origin_shift: (0, 0),
                owner: context.caller.clone(),
                lease: format!("targets-{revision:016x}"),
                snapshot: self.capture_scoped_baseline()?,
                operation_id: None,
                requested_configuration: None,
                request_identity: None,
                expected_topology_revision: None,
                pending_route: None,
            },
        };
        if journal.scoped_pending.is_some() {
            return Err(DisplayError::new(
                "scoped_recovery_required",
                "a display mutation must settle before target references change",
            ));
        }
        let existing = journal.scoped_targets.iter().position(|target| {
            target.borrowed
                && target.owner == context.caller
                && target.identity == selector
                && target.reference_id == reference_id
        });
        if releasing {
            if let Some(index) = existing {
                journal.scoped_targets.remove(index);
            }
            // Native physical targets and the normal baseline are untouched.
            return Ok((
                journal,
                json!({"ok":true,"released":true,"native_removed":false,"selector":selector,"reference_id":reference_id}),
                None,
            ));
        }
        let target = targets::select(self.platform.capture_targets()?, &selector)?;
        let binding = target.binding.ok_or_else(|| {
            DisplayError::new("capture_target_unavailable", "selected source is not ready")
        })?;
        let reply =
            json!({"ok":true,"selector":selector,"reference_id":reference_id,"binding":binding});
        if let Some(index) = existing {
            let retained = &journal.scoped_targets[index];
            if retained.target_id != target.native_id {
                return Err(DisplayError::new(
                    "capture_target_changed",
                    "retained source identity changed",
                ));
            }
            if retained.operation_id == operation_id
                && retained.request_identity.as_ref() != Some(&request_identity)
            {
                return Err(DisplayError::new(
                    "operation_conflict",
                    "borrow operation identity has different content",
                ));
            }
            if retained.borrow_binding.as_ref() != Some(&binding) {
                return Err(DisplayError::new(
                    "capture_target_changed",
                    "retained source mode, color or binding changed; release the exact reference before selecting it again",
                ));
            }
            return Ok((journal, reply, Some(selector)));
        }
        if journal.scoped_targets.len() >= scoped::MAX_SCOPED_TARGETS {
            return Err(DisplayError::new(
                "resource_exhausted",
                "protected target capacity exhausted",
            ));
        }
        if journal.scoped_targets.iter().any(|owned| {
            owned.target_id.eq_ignore_ascii_case(&target.native_id)
                && owned.borrowed
                && owned.borrow_binding.as_ref() != Some(&binding)
        }) {
            return Err(DisplayError::new(
                "target_ownership_conflict",
                "target is being independently reconfigured",
            ));
        }
        journal.scoped_targets.push(ScopedReservation {
            owner: context.caller.clone(),
            identity: selector.clone(),
            reference_id: reference_id.clone(),
            target_id: target.native_id,
            operation_id,
            operation_applied: true,
            request_identity: Some(request_identity),
            primary: false,
            borrowed: true,
            borrow_binding: Some(binding),
        });
        Ok((journal, reply, Some(selector)))
    }

    pub(super) fn finish_target_borrow(
        &mut self,
        context: Context,
        reply: Value,
        selector: String,
        active_lease: Option<String>,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        self.active_lease = active_lease;
        let expected = serde_json::from_value::<Binding>(reply["binding"].clone());
        let observed = self
            .platform
            .capture_targets()
            .and_then(|targets| targets::select(targets, &selector));
        if expected.is_err()
            || observed.as_ref().is_err()
            || observed
                .as_ref()
                .ok()
                .and_then(|target| target.binding.as_ref())
                != expected.as_ref().ok()
        {
            return complete(
                send,
                context.request,
                failure(
                    "capture_target_changed",
                    "the source changed while its protected reference was persisted; release the exact reference before retry",
                ),
            );
        }
        complete(send, context.request, reply)
    }
}
