//! Shared-journal, caller-owned display composition.
use super::*;
use crate::scoped::{self, ScopedPending, ScopedReservation};
use amaterasu_module_sdk::virtual_display;

impl<P: Platform> Pipeline<P> {
    pub(super) fn validate_scoped_owner(
        &mut self,
        incoming: Incoming,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let Operation::ScopedApply {
            operation_id,
            identity,
            reference_id,
            target,
            expected_topology_revision,
        } = &incoming.operation
        else {
            unreachable!("scoped apply validation")
        };
        for value in [
            operation_id,
            identity,
            reference_id,
            expected_topology_revision,
        ] {
            if let Err(error) = scoped::validate_identity(value) {
                return complete(send, incoming.context.request, error.payload());
            }
        }
        if target.id.is_empty()
            || target.id.len() > 4096
            || target.id.chars().any(char::is_control)
            || target.width == 0
            || target.height == 0
            || target.refresh.numerator == 0
            || target.refresh.denominator == 0
        {
            return complete(
                send,
                incoming.context.request,
                failure("invalid_request", "invalid scoped target"),
            );
        }
        let payload = serde_json::to_value(virtual_display::Operation::VerifyOwnerTarget {
            owner: incoming.context.caller.clone(),
            identity: identity.clone(),
            reference_id: reference_id.clone(),
        })
        .map_err(|error| error.to_string())?;
        self.submit_service(
            incoming.context,
            virtual_display::SERVICE,
            payload,
            Stage::ValidatingScoped(incoming.operation),
            send,
        )
    }

    pub(super) fn scoped_owner_validated(
        &mut self,
        pending: Pending,
        payload: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let Stage::ValidatingScoped(operation) = pending.stage else {
            unreachable!("scoped validation stage")
        };
        let Operation::ScopedApply {
            ref target,
            ref identity,
            ..
        } = operation
        else {
            unreachable!("scoped apply")
        };
        let proof = virtual_display::owner_target_from_reply(payload, identity);
        let verified = proof.is_ok_and(|proof| {
            proof.target_id == target.id
                && proof.specification.width == target.width
                && proof.specification.height == target.height
                && u64::from(proof.specification.refresh_millihz)
                    * u64::from(target.refresh.denominator)
                    == 1000 * u64::from(target.refresh.numerator)
        });
        if !verified {
            return complete(
                send,
                pending.context.request,
                failure(
                    "target_ownership_unverified",
                    "virtual-display provider did not verify this caller's exact target and mode",
                ),
            );
        }
        let mut context = pending.context;
        context.verified_scoped_target = Some(target.id.clone());
        self.submit(
            context,
            json!({"action":"get","key":JOURNAL_KEY}),
            Stage::Loading(operation),
            send,
        )
    }

    pub(super) fn loaded_scoped(
        &mut self,
        context: Context,
        operation: Operation,
        revision: u64,
        journal: Option<Journal>,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let result = self.prepare_scoped(&context, operation, revision, journal);
        let (journal, before, configuration, settled) = match result {
            Ok(result) => result,
            Err(error) => return complete(send, context.request, error.payload()),
        };
        if let Some(reply) = settled {
            return self.persist_scoped(context, revision, journal, reply, send);
        }
        if journal.validate().is_err()
            || serde_json::to_vec(&journal).map_or(true, |bytes| bytes.len() > 12_000)
        {
            return complete(
                send,
                context.request,
                failure(
                    "resource_exhausted",
                    "scoped recovery journal exceeds bounded storage",
                ),
            );
        }
        self.submit(
            context,
            json!({"action":"set","key":JOURNAL_KEY,"expected_revision":revision,"value":journal}),
            Stage::ScopedArmed {
                journal,
                before,
                configuration,
            },
            send,
        )
    }

    fn prepare_scoped(
        &mut self,
        context: &Context,
        operation: Operation,
        revision: u64,
        journal: Option<Journal>,
    ) -> DisplayResult<(Journal, RecoveryBaseline, Configuration, Option<Value>)> {
        let request_identity = serde_json::to_value(&operation)
            .map_err(|error| DisplayError::new("invalid_request", error.to_string()))?;
        let (operation_id, identity, reference_id, requested, expected_revision) = match operation {
            Operation::ScopedApply {
                operation_id,
                identity,
                reference_id,
                target,
                expected_topology_revision,
            } => {
                if context.verified_scoped_target.as_deref() != Some(&target.id) {
                    return Err(DisplayError::new(
                        "target_ownership_unverified",
                        "scoped target verification is missing",
                    ));
                }
                (
                    operation_id,
                    identity,
                    reference_id,
                    Some(target),
                    expected_topology_revision,
                )
            }
            Operation::ScopedRelease {
                operation_id,
                identity,
                reference_id,
                expected_topology_revision,
            } => (
                operation_id,
                identity,
                reference_id,
                None,
                expected_topology_revision,
            ),
            _ => unreachable!("scoped operations only"),
        };
        for value in [&operation_id, &identity, &reference_id, &expected_revision] {
            scoped::validate_identity(value)?;
        }
        let view = self.platform.read_configuration_snapshot()?;
        let current_revision = view["topology_revision"].as_str().ok_or_else(|| {
            DisplayError::new(
                "invalid_snapshot",
                "display provider omitted topology revision",
            )
        })?;
        let current: Configuration = serde_json::from_value(view["configuration"].clone())
            .map_err(|_| {
                DisplayError::new("invalid_snapshot", "display provider omitted configuration")
            })?;
        current.validate()?;
        let before = self.capture_scoped_baseline()?;
        if baseline_revision(&before)? != current_revision {
            return Err(DisplayError::new(
                "topology_changed",
                "display topology changed during scoped preparation",
            ));
        }
        let mut journal = journal.unwrap_or_else(|| Journal {
            normal_active: false,
            scoped_targets: Vec::new(),
            scoped_pending: None,
            owner: context.caller.clone(),
            lease: format!("scoped-{revision:016x}"),
            snapshot: before.clone(),
            operation_id: None,
            requested_configuration: None,
            request_identity: None,
            expected_topology_revision: None,
            pending_route: None,
        });
        let releasing = requested.is_none();
        let pending_retry = journal.scoped_pending.as_ref().is_some_and(|pending| {
            pending.owner == context.caller
                && pending.operation_id == operation_id
                && pending.request_identity == request_identity
        });
        let cleanup_own_pending = releasing
            && journal.scoped_pending.as_ref().is_some_and(|pending| {
                pending.owner == context.caller
                    && journal.scoped_targets.iter().any(|target| {
                        target.owner == context.caller
                            && target.identity == identity
                            && target.reference_id == reference_id
                            && target.target_id == pending.target_id
                    })
            });
        if journal.scoped_pending.is_some() && !pending_retry && !cleanup_own_pending {
            return Err(DisplayError::new(
                "scoped_recovery_required",
                "another protected target operation must settle",
            ));
        }
        let existing = journal.scoped_targets.iter().position(|target| {
            target.owner == context.caller
                && target.identity == identity
                && target.reference_id == reference_id
        });
        if !pending_retry
            && !cleanup_own_pending
            && let Some(index) = existing
        {
            let reserved = &journal.scoped_targets[index];
            if reserved.operation_id == operation_id {
                if reserved.request_identity.as_ref() != Some(&request_identity) {
                    return Err(DisplayError::new(
                        "operation_conflict",
                        "scoped operation identity already has different content",
                    ));
                }
                if requested.as_ref().is_some_and(|requested| {
                    current
                        .displays
                        .iter()
                        .any(|observed| scoped::same_setting(requested, observed))
                }) {
                    let reply = json!({"ok":true,"applied":false,"reconciled":true,"target_id":reserved.target_id,"operation_id":operation_id});
                    return Ok((journal, before, current, Some(reply)));
                }
            }
        }
        if !pending_retry && current_revision != expected_revision {
            return Err(DisplayError::new(
                "topology_changed",
                "scoped request topology revision is stale",
            ));
        }
        let target_id = if let Some(target) = &requested {
            if existing.is_some_and(|index| journal.scoped_targets[index].target_id != target.id)
                || journal.scoped_targets.iter().any(|owned| {
                    owned.target_id.eq_ignore_ascii_case(&target.id)
                        && (owned.owner != context.caller
                            || owned.identity != identity
                            || owned.reference_id != reference_id)
                })
            {
                return Err(DisplayError::new(
                    "target_ownership_conflict",
                    "target belongs to a different scoped reservation",
                ));
            }
            if existing.is_none()
                && journal.normal_active
                && baseline_contains(&journal.snapshot, &target.id)
            {
                return Err(DisplayError::new(
                    "target_ownership_conflict",
                    "target belongs to the normal display lease baseline",
                ));
            }
            target.id.clone()
        } else if let Some(index) = existing {
            journal.scoped_targets[index].target_id.clone()
        } else {
            let reply = json!({"ok":true,"released":true,"already_complete":true});
            return Ok((journal, before, current, Some(reply)));
        };
        if releasing {
            journal.scoped_targets.retain(|owned| {
                !(owned.owner == context.caller
                    && owned.identity == identity
                    && owned.reference_id == reference_id)
            });
            journal.scoped_pending = None;
            if journal.normal_active || !journal.scoped_targets.is_empty() {
                scoped::forget_target(&mut journal.snapshot, &target_id)?;
            }
            let reply = json!({"ok":true,"released":true,"native_removed":false,"target_id":target_id,"operation_id":operation_id});
            return Ok((journal, before, current, Some(reply)));
        }
        if pending_retry {
            let pending = journal
                .scoped_pending
                .as_ref()
                .expect("checked pending retry");
            if scoped::same_configuration(&pending.desired_configuration, &current) {
                let released = pending.releasing;
                self.finish_scoped_journal(&mut journal, &before)?;
                let reply = json!({"ok":true,"applied":!released,"released":released,"reconciled":true,"target_id":target_id,"operation_id":operation_id});
                return Ok((journal, before, current, Some(reply)));
            }
            if !same_peers(&pending.before_configuration, &current, &target_id) {
                return Err(DisplayError::new(
                    "scoped_recovery_required",
                    "unrelated outputs changed during an uncertain scoped operation",
                ));
            }
        }
        if existing.is_none() {
            if journal.scoped_targets.len() == scoped::MAX_SCOPED_TARGETS {
                return Err(DisplayError::new(
                    "resource_exhausted",
                    "protected target capacity exhausted",
                ));
            }
            journal.scoped_targets.push(ScopedReservation {
                owner: context.caller.clone(),
                identity,
                reference_id,
                target_id: target_id.clone(),
                operation_id: operation_id.clone(),
                request_identity: Some(request_identity.clone()),
            });
        }
        let mut desired = current.clone();
        desired
            .displays
            .retain(|display| !display.id.eq_ignore_ascii_case(&target_id));
        if let Some(target) = requested {
            desired.displays.push(target);
        }
        desired.validate()?;
        if !same_peers(&current, &desired, &target_id) {
            return Err(DisplayError::new(
                "peer_preservation_failed",
                "scoped mutation would change an unrelated output",
            ));
        }
        let mut pending_route = None;
        // Preflight native routes before recording admission. All native effects
        // are still guarded by the durable state write and the broker request.
        match &before {
            RecoveryBaseline::KScreen(snapshot) => {
                self.platform.prepare_kscreen_snapshot(&desired, snapshot)?;
            }
            RecoveryBaseline::Ccd(snapshot) => {
                if !current
                    .displays
                    .iter()
                    .any(|display| display.id.eq_ignore_ascii_case(&target_id))
                {
                    pending_route = Some(self.platform.bind_inactive(&target_id, snapshot)?);
                }
                let prepared = self.platform.prepare_preserving(
                    &desired,
                    &target_id,
                    pending_route.as_ref(),
                    TopologyShape::PreserveActive,
                )?;
                if prepared.revision()? != baseline_revision(&before)? {
                    return Err(DisplayError::new(
                        "topology_changed",
                        "native topology changed during scoped preparation",
                    ));
                }
            }
        }
        journal.scoped_pending = Some(ScopedPending {
            owner: context.caller.clone(),
            operation_id,
            request_identity,
            target_id,
            before_configuration: current,
            desired_configuration: desired.clone(),
            releasing: false,
            route: pending_route,
        });
        Ok((journal, before, desired, None))
    }

    pub(super) fn apply_scoped(
        &mut self,
        context: Context,
        revision: u64,
        mut journal: Journal,
        before: RecoveryBaseline,
        configuration: Configuration,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let result = if let Err(error) = check_request(send, &context) {
            Err(error)
        } else {
            match &before {
                RecoveryBaseline::KScreen(snapshot) => apply_kscreen_transaction(
                    &mut self.platform,
                    snapshot,
                    &configuration,
                    &mut || check_request(send, &context),
                ),
                RecoveryBaseline::Ccd(snapshot) => {
                    let pending = journal
                        .scoped_pending
                        .as_ref()
                        .expect("armed scoped target");
                    self.platform.apply_preserving(
                        &configuration,
                        snapshot,
                        &pending.target_id,
                        pending.route.as_ref(),
                        TopologyShape::PreserveActive,
                        &mut || check_request(send, &context),
                    )
                }
            }
        };
        if let Err(error) = result {
            let mut reply = error.payload();
            reply["recovery_required"] = json!(true);
            reply["scoped_pending"] = json!(true);
            return complete(send, context.request, reply);
        }
        let current = match self.capture_scoped_baseline() {
            Ok(current) => current,
            Err(error) => {
                return complete(
                    send,
                    context.request,
                    recovery_failure(error, &journal.lease),
                );
            }
        };
        let view = match self.platform.read_configuration_snapshot() {
            Ok(view) => view,
            Err(error) => {
                return complete(
                    send,
                    context.request,
                    recovery_failure(error, &journal.lease),
                );
            }
        };
        let observed = serde_json::from_value::<Configuration>(view["configuration"].clone());
        if observed.as_ref().is_err()
            || !scoped::same_configuration(&configuration, observed.as_ref().unwrap())
        {
            return complete(
                send,
                context.request,
                failure(
                    "scoped_recovery_required",
                    "native scoped readback does not match the admitted complete configuration",
                ),
            );
        }
        let pending = journal
            .scoped_pending
            .as_ref()
            .expect("durably armed scoped operation");
        let reply = json!({"ok":true,"applied":!pending.releasing,"released":pending.releasing,"target_id":pending.target_id,"operation_id":pending.operation_id});
        if let Err(error) = self.finish_scoped_journal(&mut journal, &current) {
            return complete(
                send,
                context.request,
                recovery_failure(error, &journal.lease),
            );
        }
        self.persist_scoped(context, revision, journal, reply, send)
    }

    fn finish_scoped_journal(
        &mut self,
        journal: &mut Journal,
        current: &RecoveryBaseline,
    ) -> DisplayResult<()> {
        let pending = journal
            .scoped_pending
            .take()
            .ok_or_else(|| DisplayError::new("invalid_journal", "scoped operation is missing"))?;
        if pending.releasing {
            journal.scoped_targets.retain(|owned| {
                !(owned.owner == pending.owner && owned.target_id == pending.target_id)
            });
            scoped::forget_target(&mut journal.snapshot, &pending.target_id)?;
        } else {
            if let Some(target) = journal.scoped_targets.iter_mut().find(|target| {
                target.owner == pending.owner && target.target_id == pending.target_id
            }) {
                target.operation_id = pending.operation_id;
                target.request_identity = Some(pending.request_identity);
            }
            if !journal.normal_active {
                journal.snapshot = current.clone();
            }
        }
        Ok(())
    }

    fn persist_scoped(
        &mut self,
        context: Context,
        revision: u64,
        journal: Journal,
        reply: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if !journal.normal_active && journal.scoped_targets.is_empty() {
            return self.clear(context, revision, reply, send);
        }
        if let Err(error) = journal.validate() {
            return complete(send, context.request, error.payload());
        }
        let active_lease = journal.normal_active.then(|| journal.lease.clone());
        self.submit(
            context,
            json!({"action":"set","key":JOURNAL_KEY,"expected_revision":revision,"value":journal}),
            Stage::Saved {
                reply,
                active_lease,
            },
            send,
        )
    }

    fn capture_scoped_baseline(&mut self) -> DisplayResult<RecoveryBaseline> {
        match self.platform.capture_kscreen_snapshot()? {
            Some(snapshot) => Ok(RecoveryBaseline::KScreen(snapshot)),
            None => self.platform.snapshot().map(RecoveryBaseline::Ccd),
        }
    }

    pub(super) fn validate_protected_configuration(
        &mut self,
        configuration: &Configuration,
        targets: &[ScopedReservation],
    ) -> DisplayResult<()> {
        let view = self.platform.read_configuration_snapshot()?;
        let current: Configuration = serde_json::from_value(view["configuration"].clone())
            .map_err(|_| {
                DisplayError::new(
                    "invalid_snapshot",
                    "provider omitted current display configuration",
                )
            })?;
        for target in targets {
            let observed = current
                .displays
                .iter()
                .find(|display| display.id.eq_ignore_ascii_case(&target.target_id));
            let requested = configuration
                .displays
                .iter()
                .find(|display| display.id.eq_ignore_ascii_case(&target.target_id));
            if observed
                .zip(requested)
                .is_none_or(|(observed, requested)| !scoped::same_setting(observed, requested))
            {
                return Err(DisplayError::new(
                    "protected_target_conflict",
                    "normal display request would change an independently retained target",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn rebase_protected_restore(&mut self, journal: &mut Journal) -> DisplayResult<()> {
        if journal.scoped_targets.is_empty() {
            return Ok(());
        }
        let current = self.capture_scoped_baseline()?;
        for target in &journal.scoped_targets {
            scoped::merge_current_target(&mut journal.snapshot, &current, &target.target_id)?;
        }
        Ok(())
    }

    pub(super) fn retain_scoped_after_error(
        &mut self,
        context: Context,
        revision: u64,
        mut journal: Journal,
        reply: Value,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        journal.snapshot = match self.capture_scoped_baseline() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return complete(
                    send,
                    context.request,
                    recovery_failure(error, &journal.lease),
                );
            }
        };
        journal.normal_active = false;
        journal.owner = "amaterasu.display.scoped".into();
        journal.operation_id = None;
        journal.requested_configuration = None;
        journal.request_identity = None;
        journal.expected_topology_revision = None;
        journal.pending_route = None;
        self.persist_scoped(context, revision, journal, reply, send)
    }

    pub(super) fn retain_scoped_after_restore(
        &mut self,
        context: Context,
        revision: u64,
        mut journal: Journal,
        lease: String,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        journal.snapshot = match self.capture_scoped_baseline() {
            Ok(snapshot) => snapshot,
            Err(error) => return complete(send, context.request, recovery_failure(error, &lease)),
        };
        journal.normal_active = false;
        journal.owner = "amaterasu.display.scoped".into();
        journal.operation_id = None;
        journal.requested_configuration = None;
        journal.request_identity = None;
        journal.expected_topology_revision = None;
        journal.pending_route = None;
        self.persist_scoped(
            context,
            revision,
            journal,
            json!({"ok":true,"restored":true,"lease":lease,"protected_targets_retained":true}),
            send,
        )
    }
}

fn baseline_revision(baseline: &RecoveryBaseline) -> DisplayResult<String> {
    match baseline {
        RecoveryBaseline::Ccd(snapshot) => snapshot.revision(),
        RecoveryBaseline::KScreen(snapshot) => snapshot.revision(),
    }
}

fn baseline_contains(baseline: &RecoveryBaseline, target_id: &str) -> bool {
    match baseline {
        RecoveryBaseline::Ccd(snapshot) => snapshot
            .paths
            .iter()
            .any(|path| path.id.eq_ignore_ascii_case(target_id)),
        RecoveryBaseline::KScreen(snapshot) => snapshot.outputs.iter().any(|output| {
            output.id.eq_ignore_ascii_case(target_id) && output.connected && output.enabled
        }),
    }
}

fn same_peers(before: &Configuration, after: &Configuration, target_id: &str) -> bool {
    let without_target = |configuration: &Configuration| Configuration {
        displays: configuration
            .displays
            .iter()
            .filter(|display| !display.id.eq_ignore_ascii_case(target_id))
            .cloned()
            .collect(),
    };
    scoped::same_configuration(&without_target(before), &without_target(after))
}
