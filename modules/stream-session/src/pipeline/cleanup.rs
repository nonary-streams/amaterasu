use super::input::*;
use super::responses::*;
use super::*;

impl Pipeline {
    pub(super) fn consume_cleanup(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        payload: Value,
        now: u64,
        send: &mut Send<'_>,
    ) -> bool {
        match job.step {
            Step::ResolveLifecyclePreparation => {
                let preparation = record
                    .restart_lifecycle_preparation
                    .as_ref()
                    .expect("preparation recovery");
                let lookup = preparation.lookup(
                    record
                        .config
                        .lifecycle_session_target
                        .as_ref()
                        .expect("recovery session"),
                );
                let resolution = serde_json::from_value::<
                    amaterasu_extension_api::StreamPreparationResolution,
                >(payload)
                .ok()
                .filter(|response| {
                    amaterasu_extension_api::validate_stream_preparation_resolution(
                        &lookup, response,
                    )
                    .is_ok()
                });
                match resolution {
                    Some(response) => match response.status {
                        amaterasu_extension_api::StreamPreparationResolutionStatus::Succeeded => {
                            record.restart_lifecycle_target = response.target;
                            record.restart_lifecycle_preparation = None;
                        }
                        amaterasu_extension_api::StreamPreparationResolutionStatus::Missing => {
                            // Only the authenticated coordinator can prove this
                            // old preparation cannot commit after restart.
                            record.restart_lifecycle_preparation = None;
                            record.config.lifecycle_session_target = None;
                            record.lifecycle_fenced = false;
                        }
                        amaterasu_extension_api::StreamPreparationResolutionStatus::Pending => {}
                    },
                    None => {
                        record.cleanup_error = Some("lifecycle_preparation_unconfirmed");
                        job.lifecycle_retire_tried = true;
                    }
                }
            }
            Step::ConfirmControlDisconnect => {
                record.control_disconnect_checked = true;
                if payload["ok"] == true
                    && let Some(id) = record.resources.get(&Resource::Control)
                    && let Some(status) = component_status(&payload, *id)
                    && status["state"] == "failed"
                {
                    record.authenticated_disconnect_reason = match status["failure"].as_str() {
                        Some("peer_disconnected") => Some("peer_disconnected"),
                        Some("authenticated_peer_idle") => Some("authenticated_peer_idle"),
                        Some("peer_timeout") => Some("peer_timeout"),
                        _ => None,
                    };
                }
            }
            Step::PauseLifecycle => {
                job.lifecycle_pause_tried = true;
                let target = record
                    .start
                    .as_ref()
                    .and_then(|start| start.lifecycle_target.as_ref());
                let plan_revision = record
                    .start
                    .as_ref()
                    .and_then(|start| start.lifecycle_candidate.as_ref())
                    .map(|candidate| candidate.plan_revision);
                if payload["target"] == json!(target)
                    && payload["plan_revision"] == json!(plan_revision)
                    && payload["fenced"] == true
                    && matches!(payload["status"].as_str(), Some("pending" | "succeeded"))
                {
                    record.lifecycle_fenced = true;
                } else {
                    record.cleanup_error = Some("lifecycle_fence_unconfirmed");
                }
            }
            Step::RetireLifecycle | Step::RecoverLifecycle => {
                let target = record
                    .start
                    .as_ref()
                    .and_then(|start| start.lifecycle_target.as_ref())
                    .or(record.restart_lifecycle_target.as_ref());
                let plan_revision = record
                    .start
                    .as_ref()
                    .and_then(|start| start.lifecycle_candidate.as_ref())
                    .map(|candidate| candidate.plan_revision);
                if payload["status"] == "pending"
                    && payload["target"] == json!(target)
                    && payload["plan_revision"] == json!(plan_revision)
                {
                    job.lifecycle_lookup = true;
                } else {
                    job.lifecycle_retire_tried = true;
                    job.lifecycle_lookup = false;
                    let proof = target.and_then(|target| {
                        amaterasu_extension_api::stream_retirement_proof(
                            &payload,
                            target,
                            record
                                .start
                                .as_ref()
                                .and_then(|start| start.lifecycle_shared_source.as_ref())
                                .or_else(|| {
                                    record.shared_capture.as_ref().map(|source| &source.stream)
                                }),
                        )
                        .ok()
                    });
                    if payload["status"] == "succeeded"
                        && payload["target"] == json!(target)
                        && payload["plan_revision"] == json!(plan_revision)
                        && let Some(proof) = proof
                    {
                        record.lifecycle_retired = proof.resources_released;
                        record.lifecycle_retirement_proof = Some(proof);
                    } else {
                        record.cleanup_error = Some("lifecycle_retirement_unconfirmed");
                    }
                }
            }
            Step::FindOwnedVirtualDisplay | Step::RemoveOwnedVirtualDisplay => {
                if payload["error"]["code"] == "startup_reconciliation_pending" {
                    return true;
                }
                if super::virtual_display::absent(record, &payload) {
                    record.shared_capture_attached = false;
                    record.virtual_display_attempted = false;
                    record.virtual_display_output = None;
                    if record.cleanup_error == Some("virtual_display_remove_unconfirmed") {
                        record.cleanup_error = None;
                    }
                    job.virtual_display_tried = true;
                } else if payload["ok"] == true {
                    if payload["monitors"].as_array().is_some_and(|monitors| {
                        monitors
                            .iter()
                            .any(|monitor| monitor["state"] == "cleanup_failed")
                    }) {
                        record.cleanup_error = Some("virtual_display_cleanup_failed");
                        job.virtual_display_tried = true;
                    } else {
                        job.virtual_display_removing = true;
                    }
                } else {
                    record.cleanup_error = Some("virtual_display_cleanup_failed");
                    job.virtual_display_tried = true;
                }
            }
            Step::FindDisplay => {
                if super::display::accept_status(record, &payload) {
                    if record.display_lease.is_none() {
                        job.display_tried = true;
                    }
                } else {
                    record.cleanup_error = Some("display_status_unavailable");
                    job.display_tried = true;
                }
            }
            Step::RestoreDisplay | Step::RecoverDisplay => {
                if super::display::accept_restore(record, &payload) {
                    job.display_tried = true;
                } else {
                    record.cleanup_error = Some("display_restore_failed");
                    job.display_tried = true;
                }
            }
            Step::StatusVirtualDisplay => {
                use super::virtual_display::ReferenceStatus;
                match super::virtual_display::accept_status(record, &payload) {
                    ReferenceStatus::Pending if record.virtual_display_remove_attempted => {
                        job.step = Step::StatusVirtualDisplay;
                    }
                    ReferenceStatus::Failed
                        if record.virtual_display_remove_attempted
                            && record.virtual_display_state == "cleanup_failed" =>
                    {
                        record.cleanup_error = Some("virtual_display_cleanup_failed");
                        job.virtual_display_tried = true;
                    }
                    ReferenceStatus::Missing if record.virtual_display_status_verified => {
                        record.virtual_display_attempted = false;
                        record.virtual_display_remove_attempted = false;
                        job.virtual_display_tried = true;
                    }
                    ReferenceStatus::Ready | ReferenceStatus::Pending | ReferenceStatus::Failed
                        if record.virtual_display_status_verified
                            && record.virtual_display_reference_owned =>
                    {
                        if record.virtual_display_remove_attempted {
                            record.cleanup_error = Some("virtual_display_remove_unconfirmed");
                            job.virtual_display_tried = true;
                        } else {
                            job.step = Step::RemoveVirtualDisplay;
                        }
                    }
                    _ => {
                        record.cleanup_error = Some("virtual_display_status_unavailable");
                        job.virtual_display_tried = true;
                    }
                }
            }
            Step::RemoveVirtualDisplay => {
                if super::virtual_display::accept_remove(record, &payload) {
                    record.virtual_display_attempted = false;
                    record.virtual_display_remove_attempted = false;
                    job.virtual_display_tried = true;
                } else {
                    record.cleanup_error = Some("virtual_display_remove_failed");
                    // The removal response may have been lost after releasing the
                    // reference. Confirm through status before reporting cleanup.
                    job.step = Step::StatusVirtualDisplay;
                }
            }
            Step::CreateVirtualDisplay => {
                if payload["ok"] != true {
                    record.virtual_display_create_error = payload["code"]
                        .as_str()
                        .or_else(|| payload["error"]["code"].as_str())
                        .map(str::to_owned);
                }
                job.step = Step::StatusVirtualDisplay;
            }
            Step::SnapshotDisplay => {
                record.cleanup_error = Some("display_snapshot_unavailable");
                job.virtual_display_tried = true;
            }
            Step::ApplyDisplay => {
                super::display::accept_apply(record, &payload);
                job.step = Step::FindDisplay;
            }
            Step::FindInput => {
                if input_missing(&payload) || input_released(record, &payload) {
                    clear_input(record);
                    job.input_tried = true;
                } else if adopt_input_for_cleanup(record, &payload).is_err() {
                    return self.cleanup_rejected(record, job, send);
                }
            }
            Step::ReleaseInput => {
                if accept_input_release(record, &payload).is_ok() {
                    job.input_tried = true;
                } else {
                    return self.cleanup_rejected(record, job, send);
                }
            }
            Step::Open(resource) => {
                if let Some(id) = allocation_id(record, resource, &payload) {
                    record.resources.insert(resource, id);
                }
            }
            Step::Find(resource) => {
                if let Some(id) = allocation_id(record, resource, &payload) {
                    record.resources.insert(resource, id);
                    record.attempted.insert(resource);
                    job.tried.remove(&resource);
                } else if missing(&payload) {
                    record.attempted.remove(&resource);
                    if resource == Resource::Video && record.lifecycle_fenced {
                        record.lifecycle_retirement_receipt = Some(
                            json!({"service":record.service(Resource::Video),"operation_id":record.operation(resource),"response":{"code":payload["code"]}}),
                        );
                    }
                } else {
                    return self.cleanup_rejected(record, job, send);
                }
            }
            Step::Close(resource) => {
                if payload["ok"] != true && !missing(&payload) {
                    return self.cleanup_rejected(record, job, send);
                }
                if resource == Resource::Video && !missing(&payload) {
                    job.confirming = Some(resource);
                } else {
                    record.resources.remove(&resource);
                    record.attempted.remove(&resource);
                    job.tried.insert(resource);
                }
            }
            Step::AwaitClosed(resource) => {
                if missing(&payload) {
                    record.resources.remove(&resource);
                    record.attempted.remove(&resource);
                    job.tried.insert(resource);
                    job.confirming = None;
                    if resource == Resource::Video && record.lifecycle_fenced {
                        record.lifecycle_retirement_receipt = Some(
                            json!({"service":record.service(Resource::Video),"operation_id":record.operation(resource),"response":{"code":payload["code"]}}),
                        );
                    }
                } else if allocation_id(record, resource, &payload).is_none()
                    || payload["state"] == "cleanup_failed"
                {
                    return self.cleanup_rejected(record, job, send);
                }
            }
            Step::StopSession => {
                if session_response(record, &payload).is_none_or(|value| {
                    !matches!(value["state"].as_str(), Some("stopping" | "stopped"))
                }) {
                    return self.cleanup_rejected(record, job, send);
                }
                job.step = Step::FinishSession;
                // Preserve this explicit transition rather than restarting cleanup selection.
                job.pending = Some(self.cleanup_rpc(record, job));
            }
            Step::FinishSession => {
                if session_response(record, &payload).is_none_or(|value| {
                    value["state"] != "stopped"
                        || !value["leases"]
                            .as_object()
                            .is_some_and(|leases| leases.is_empty())
                }) {
                    return self.cleanup_rejected(record, job, send);
                }
                let (outcome, reason) = match record.outcome {
                    State::Stopped => ("stopped", None),
                    State::Cancelled => ("cancelled", record.error),
                    _ => ("failed", record.error),
                };
                record.history.teardown_confirmed(now, outcome, reason);
                record.host_metrics.teardown_confirmed(record.id);
                record.session_effects.teardown_confirmed(record.id);
                self.stop_tray_session(record.id);
                record.resources.remove(&Resource::Session);
                record.attempted.remove(&Resource::Session);
            }
            _ => {}
        }
        true
    }

    fn cleanup_rpc(&mut self, record: &Record, job: &Job) -> rpc::Pending {
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("bounded operation lifetime");
        let (service, payload) = job.command(record);
        rpc::Pending::new(
            &service,
            payload,
            format!("ss-{:x}-{}", record.id, self.sequence),
            None,
        )
    }

    pub(super) fn cleanup_rejected(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        send: &mut Send<'_>,
    ) -> bool {
        record.cleanup_error = Some("component_cleanup_failed");
        match job.step {
            Step::RemoveOwnedVirtualDisplay => {
                // The provider may have committed the removal before its reply
                // was lost. Observe the same stable reference without sending
                // another native mutation.
                record.cleanup_error = Some("virtual_display_remove_unconfirmed");
                job.virtual_display_removing = true;
                true
            }
            Step::FindOwnedVirtualDisplay => {
                record.cleanup_error = Some("virtual_display_cleanup_unavailable");
                job.virtual_display_tried = true;
                true
            }
            Step::ResolveLifecyclePreparation => {
                record.cleanup_error = Some("lifecycle_preparation_unconfirmed");
                job.lifecycle_retire_tried = true;
                true
            }
            Step::FindDisplay | Step::RestoreDisplay | Step::RecoverDisplay => {
                record.cleanup_error = Some("display_cleanup_unavailable");
                job.display_tried = true;
                true
            }
            Step::StatusVirtualDisplay => {
                record.cleanup_error = Some("virtual_display_status_unavailable");
                job.virtual_display_tried = true;
                true
            }
            Step::RemoveVirtualDisplay => {
                record.cleanup_error = Some("virtual_display_remove_unconfirmed");
                // Reconcile the same stable reference before declaring cleanup.
                record.virtual_display_remove_attempted = true;
                job.step = Step::StatusVirtualDisplay;
                true
            }
            Step::ReleaseInput => {
                record.input_lookup_required = true;
                job.input_tried = false;
                true
            }
            Step::FindInput => {
                job.input_tried = true;
                true
            }
            Step::Find(resource) | Step::Close(resource) | Step::AwaitClosed(resource)
                if resource != Resource::Session =>
            {
                job.confirming = None;
                job.tried.insert(resource);
                true
            }
            _ => {
                record.state = State::CleanupFailed;
                self.reply(record, job, send);
                false
            }
        }
    }

    pub(super) fn finish_cleanup(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        send: &mut Send<'_>,
    ) -> bool {
        let clean = record.attempted.is_empty()
            && record.restart_lifecycle_preparation.is_none()
            && !record.shared_capture_attached
            && record.uncertain.is_none()
            && !record.input_attempted
            && record.input_lease.is_none()
            && !record.virtual_display_attempted
            && !record.display_attempted
            && record.display_lease.is_none()
            && !record.virtual_display_reference_owned
            && (record.restart_lifecycle_target.is_none() || record.lifecycle_consumers_retired())
            && (record
                .start
                .as_ref()
                .is_none_or(|start| start.lifecycle_target.is_none())
                || record.lifecycle_consumers_retired());
        if clean && self.journal.document.entries.contains_key(&record.id) {
            let document = self.journal.without_record(record.id);
            job.journal_resume = Some(Step::WaitDrain);
            job.journal_payload = Some(self.journal.set_payload(&document));
            job.journal_document = Some(document);
            job.journal_operation_id = Some(self.journal.op_id(self.sequence.saturating_add(1)));
            job.step = Step::PersistJournal;
            return true;
        }
        record.state = if clean {
            record.cleanup_error = None;
            record.outcome
        } else {
            State::CleanupFailed
        };
        self.reply(record, job, send);
        false
    }
}
