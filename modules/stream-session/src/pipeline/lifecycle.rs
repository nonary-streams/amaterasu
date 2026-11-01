use super::input::*;
use super::responses::*;
use super::*;

impl Pipeline {
    pub(super) fn advance(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        now: u64,
        send: &mut Send<'_>,
        operations: &mut NativeOperations<'_, '_>,
    ) -> bool {
        if !job.cleanup && record.state == State::Stopping && record.outcome == State::Failed {
            job.fail(record, record.error.unwrap_or("media_failed"), now);
        }
        if !job.cleanup
            && record.state != State::Stopping
            && job.step != Step::PersistJournal
            && job
                .request
                .is_some_and(|request| !rpc::active(send, request))
        {
            job.fail(record, "request_cancelled", now);
        }
        if now >= job.deadline {
            if job.cleanup {
                if let Some(mut pending) = job.pending.take() {
                    pending.cancel();
                    record.uncertain = Some(pending);
                    record.uncertain_step = Some(job.step);
                }
                record.cleanup_error = Some("cleanup_deadline");
                record.state = State::CleanupFailed;
                self.reply(record, job, send);
                return false;
            }
            if matches!(
                job.step,
                Step::ObserveActiveBinding
                    | Step::ValidateActiveBinding
                    | Step::NotifyCaptureInvalidated
            ) {
                record.plane = None;
                record.input_events.clear();
            }
            job.fail(record, "operation_deadline", now);
        }
        if let Some(mut pending) = job.pending.take() {
            match pending.poll(send, now) {
                rpc::Progress::Waiting => {
                    job.pending = Some(pending);
                    return true;
                }
                rpc::Progress::Unresolved => {
                    if job.step == Step::ProbeExtensionSource {
                        let _ = pending.shutdown(send);
                        record.extension_checked = true;
                        return self.deliver_success(record, job, now, send);
                    }
                    if job.step == Step::PersistJournal {
                        self.journal.failed = true;
                        self.recovery_blocked = true;
                        record.cleanup_error = Some("lifecycle_journal_write_unconfirmed");
                        record.state = State::CleanupFailed;
                        self.reply(record, job, send);
                        return false;
                    }
                    if job.step == Step::ApplyLimiter {
                        let settled = pending.shutdown(send);
                        if !settled {
                            record.uncertain = Some(pending);
                            record.uncertain_step = Some(job.step);
                        }
                        record.limiter_state = "apply_uncertain".into();
                        job.fail(record, "frame_limiter_apply_uncertain", now);
                        return true;
                    }
                    if job.step == Step::ApplyDisplay {
                        let settled = pending.shutdown(send);
                        if !settled {
                            record.uncertain = Some(pending);
                            record.uncertain_step = Some(job.step);
                        }
                        record.display_state = "apply_uncertain".into();
                        job.fail(record, "display_apply_uncertain", now);
                        return true;
                    }
                    if matches!(
                        job.step,
                        Step::CreateVirtualDisplay
                            | Step::StatusVirtualDisplay
                            | Step::SnapshotDisplay
                            | Step::RemoveVirtualDisplay
                    ) {
                        let settled = pending.shutdown(send);
                        if !settled {
                            record.uncertain = Some(pending);
                            record.uncertain_step = Some(job.step);
                        }
                        record.virtual_display_state = "request_uncertain".into();
                        job.fail(record, "virtual_display_request_uncertain", now);
                        return true;
                    }
                    record.uncertain = Some(pending);
                    record.uncertain_step = Some(job.step);
                    record.plane = None;
                    record.audio_recovery_needed = false;
                    record.cleanup_error = Some("request_cancellation_unconfirmed");
                    record.state = State::CleanupFailed;
                    self.reply(record, job, send);
                    return false;
                }
                rpc::Progress::Result(payload) => {
                    if matches!(
                        job.step,
                        Step::ProbeExtensionSource
                            | Step::ValidateLifecycleCandidate
                            | Step::ObserveLifecycleCapture
                            | Step::AckLifecycleCapture
                            | Step::StartLifecycle
                            | Step::PauseLifecycle
                            | Step::RetireLifecycle
                            | Step::ObserveActiveBinding
                            | Step::ValidateActiveBinding
                            | Step::NotifyCaptureInvalidated
                            | Step::IssueLifecycleInputBinding
                            | Step::AckLifecycleInput
                    ) && !pending.release_result(send)
                    {
                        job.pending = Some(pending);
                        return true;
                    }
                    return self.consume(record, job, payload, now, send, operations);
                }
                rpc::Progress::Failed => {
                    if job.step == Step::ProbeExtensionSource {
                        record.extension_checked = true;
                        return self.deliver_success(record, job, now, send);
                    }
                    if matches!(
                        job.step,
                        Step::ObserveActiveBinding | Step::ValidateActiveBinding
                    ) {
                        record.plane = None;
                        record.input_events.clear();
                        record.lifecycle_invalid_reason = Some("capture_validation_unavailable");
                        job.lifecycle_lookup = false;
                        job.step = Step::NotifyCaptureInvalidated;
                        return true;
                    }
                    if job.step == Step::ObserveFeedbackSource {
                        retry_feedback_source_observation(record, now);
                        return false;
                    }
                    if matches!(job.step, Step::FeedbackPeek | Step::FeedbackAck) {
                        if job.step == Step::FeedbackPeek {
                            record.feedback_ack_through = None;
                        }
                        record.feedback_poll_at = now.saturating_add(100);
                        return false;
                    } else if job.step == Step::PersistJournal {
                        self.journal.failed = true;
                        self.recovery_blocked = true;
                        if job.cleanup {
                            record.cleanup_error = Some("lifecycle_journal_write_failed");
                            record.state = State::CleanupFailed;
                            self.reply(record, job, send);
                            return false;
                        }
                        job.fail(record, "lifecycle_journal_write_failed", now);
                        return true;
                    }
                    if job.settling_allocation {
                        job.settling_allocation = false;
                        return true;
                    }
                    if job.cleanup {
                        if job.step == Step::ConfirmControlDisconnect {
                            record.control_disconnect_checked = true;
                            return true;
                        }
                        if matches!(job.step, Step::PauseLifecycle | Step::RetireLifecycle) {
                            if job.step == Step::PauseLifecycle {
                                job.lifecycle_pause_tried = true;
                            } else {
                                job.lifecycle_retire_tried = true;
                            }
                            record.cleanup_error = Some("lifecycle_cleanup_unavailable");
                            return true;
                        }
                        return if matches!(
                            job.step,
                            Step::Find(_)
                                | Step::Close(_)
                                | Step::AwaitClosed(_)
                                | Step::StopSession
                                | Step::FinishSession
                                | Step::FindInput
                                | Step::ReleaseInput
                                | Step::FindLimiter
                                | Step::RestoreLimiter
                                | Step::FindOwnedVirtualDisplay
                                | Step::RemoveOwnedVirtualDisplay
                                | Step::FindDisplay
                                | Step::RestoreDisplay
                                | Step::RecoverDisplay
                                | Step::StatusVirtualDisplay
                                | Step::RemoveVirtualDisplay
                        ) {
                            self.cleanup_rejected(record, job, send)
                        } else {
                            true
                        };
                    }
                    if matches!(job.step, Step::ApplyLimiter | Step::FindLimiter) {
                        record.limiter_state = "runtime_unavailable".into();
                        if job.step == Step::ApplyLimiter && pending.admission_rejected() {
                            record.limiter_attempted = false;
                            job.step = Step::AcquireOutput;
                            return true;
                        }
                        job.step = if job.step == Step::ApplyLimiter {
                            Step::FindLimiter
                        } else {
                            Step::AcquireOutput
                        };
                        return true;
                    }
                    if job.step == Step::CreateVirtualDisplay {
                        record.virtual_display_create_error = Some("dependency_unavailable".into());
                        job.step = Step::StatusVirtualDisplay;
                        return true;
                    }
                    if matches!(job.step, Step::StatusVirtualDisplay | Step::SnapshotDisplay) {
                        record.virtual_display_state = "dependency_unavailable".into();
                        job.fail(record, "virtual_display_unavailable", now);
                        return true;
                    }
                    if matches!(job.step, Step::CheckDisplay | Step::ApplyDisplay) {
                        record.display_state = "runtime_unavailable".into();
                        if job.step == Step::ApplyDisplay && !pending.admission_rejected() {
                            job.step = Step::FindDisplay;
                        } else if record.state == State::Preparing {
                            job.fail(record, "display_unavailable", now);
                        } else {
                            record.display_attempted = false;
                            job.step = if record.config.frame_limiter_settings.is_some()
                                && !record.limiter_attempted
                            {
                                Step::ApplyLimiter
                            } else {
                                Step::AcquireOutput
                            };
                        }
                        return true;
                    }
                    if job.step == Step::FindDisplay {
                        record.display_state = "status_unavailable".into();
                        if record.state == State::Preparing {
                            job.fail(record, "display_status_unverified", now);
                            return true;
                        }
                        job.step = if record.config.frame_limiter_settings.is_some()
                            && !record.limiter_attempted
                        {
                            Step::ApplyLimiter
                        } else {
                            Step::AcquireOutput
                        };
                        return true;
                    }
                    job.fail(record, "dependency_unavailable", now);
                }
            }
        }
        let mut transferred_display_restore = false;
        if job.cleanup && job.step != Step::PersistJournal {
            if record.uncertain.is_some() {
                return self.finish_cleanup(record, job, send);
            }
            transferred_display_restore = self.transfer_shared_display_restore(record);
            let Some(step) = job.next_cleanup(record) else {
                return self.finish_cleanup(record, job, send);
            };
            job.step = step;
        }
        if job.step == Step::RumbleDrain {
            if record.rumble_pending.is_empty() {
                job.rumble_tried = true;
            } else if record
                .rumble_shutdown_deadline
                .is_some_and(|deadline| now >= deadline)
            {
                record.rumble_stop_unconfirmed = true;
                record.rumble_pending.clear();
                record.rumble_active_until.clear();
                job.rumble_tried = true;
            } else {
                return true;
            }
            if job.cleanup {
                record.plane = None;
                if let Some(step) = job.next_cleanup(record) {
                    job.step = step;
                } else {
                    return self.finish_cleanup(record, job, send);
                }
            } else {
                job.step = Step::ReleaseInput;
            }
        }
        if job.step == Step::WaitDrain {
            if record
                .plane
                .as_ref()
                .is_some_and(|plane| plane.is_drained())
            {
                job.step = Step::TransportStatus;
            } else {
                return true;
            }
        }
        if !job.cleanup
            && job.step == Step::StatusVirtualDisplay
            && now < record.virtual_display_next_poll_at
        {
            return true;
        }
        if let Step::Open(resource) = job.step {
            record.attempted.insert(resource);
        }
        if job.step == Step::AcquireOutput
            && record
                .start
                .as_ref()
                .is_some_and(|start| start.lifecycle_target.is_some())
            && !record.lifecycle_candidate_validated
        {
            job.step = Step::ValidateLifecycleCandidate;
        }
        if job.step == Step::ApplyLimiter {
            record.limiter_attempted = true;
        }
        if job.step == Step::AcquireOutput
            && super::virtual_display::requested(record)
            && record.virtual_display_output.is_none()
        {
            job.step = Step::CreateOwnedVirtualDisplay;
        }
        if job.step == Step::CreateOwnedVirtualDisplay {
            record.virtual_display_attempted = true;
        }
        if job.step == Step::ApplyDisplay {
            record.display_attempted = true;
        }
        if job.step == Step::CreateVirtualDisplay {
            record.virtual_display_attempted = true;
            record.virtual_display_status_verified = false;
            record.virtual_display_state = "creating".into();
        }
        if job.step == Step::RemoveVirtualDisplay {
            record.virtual_display_status_verified = false;
            record.virtual_display_remove_attempted = true;
        }
        if job.step == Step::AcquireInput {
            record.input_attempted = true;
            record.input_request_started_at = now;
        }
        if job.step == Step::RenewInput {
            record.input_request_started_at = now;
        }
        if job.step == Step::ReleaseInput {
            record.input_lookup_required = true;
        }
        if job.step == Step::InputBatch {
            record.input_inflight = next_input_batch_size(record);
        }
        if job.step != Step::PersistJournal
            && (transferred_display_restore
                || (!job.cleanup && !self.journal.matches_record(record)))
        {
            let mut document = match self.journal.with_record(record) {
                Ok(document) => document,
                Err(code) => {
                    job.fail(record, code, now);
                    return true;
                }
            };
            if transferred_display_restore {
                // The recipient must durably inherit the exact topology operation
                // before the original monitor reference can be released.
                for candidate in self.records.values() {
                    if document.entries.contains_key(&candidate.id) {
                        document
                            .entries
                            .insert(candidate.id, journal::Entry::from_record(candidate));
                    }
                }
            }
            job.journal_resume = Some(job.step);
            job.journal_payload = Some(self.journal.set_payload(&document));
            job.journal_document = Some(document);
            job.journal_operation_id = Some(self.journal.op_id(self.sequence.saturating_add(1)));
            job.step = Step::PersistJournal;
        }
        let (service, payload) = job.command(record);
        self.sequence = match self.sequence.checked_add(1) {
            Some(value) => value,
            None => {
                job.fail(record, "operation_counter_exhausted", now);
                return true;
            }
        };
        let parent =
            if job.cleanup || record.state == State::Stopping || job.step == Step::PersistJournal {
                None
            } else {
                job.request
            };
        let operation_id = job
            .journal_operation_id
            .clone()
            .unwrap_or_else(|| format!("ss-{:x}-{}", record.id, self.sequence));
        let pending = rpc::Pending::new(&service, payload, operation_id, parent);
        job.pending = Some(if job.step == Step::RecoverAudio {
            pending.retry_uncertain_admission()
        } else {
            pending
        });
        true
    }

    fn consume(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        payload: Value,
        now: u64,
        send: &mut Send<'_>,
        operations: &mut NativeOperations<'_, '_>,
    ) -> bool {
        if job.step == Step::ProbeExtensionSource {
            record.extension_checked = true;
            if let Some(route) = &record.extension_route
                && payload["ok"] == true
                && payload["source_id"].as_u64() == Some(route.source_id)
                && payload["encoder"] == json!(route.encoder)
                && payload["profile"] == json!(route.profile)
                && let Some(media) = record.resources.get(&Resource::Media)
            {
                record.extension_limits = (operations.probe)(route, *media, &record.config).ok();
            }
            return self.deliver_success(record, job, now, send);
        }
        if job.step == Step::PersistJournal {
            let Some(document) = job.journal_document.take() else {
                self.journal.failed = true;
                return self.journal_write_failed(record, job, now, send);
            };
            if self
                .journal
                .validate_saved_response(&payload, document)
                .is_err()
            {
                self.journal.failed = true;
                return self.journal_write_failed(record, job, now, send);
            }
            job.journal_payload = None;
            job.journal_operation_id = None;
            job.step = job.journal_resume.take().unwrap_or(Step::WaitDrain);
            return true;
        }
        if job.cleanup {
            return self.consume_cleanup(record, job, payload, now, send);
        }
        if job.step == Step::NotifyCaptureInvalidated {
            job.fail(
                record,
                record
                    .lifecycle_invalid_reason
                    .unwrap_or("capture_binding_invalid"),
                now,
            );
            return true;
        }
        if job.step == Step::ObserveActiveBinding {
            if payload["ok"] != true
                || payload["stream"] != record.resources[&Resource::Video]
                || record.lifecycle_observation.as_ref() != Some(&payload["observed_capture"])
            {
                record.lifecycle_invalid_reason = Some("capture_binding_invalid");
                record.plane = None;
                record.input_events.clear();
                job.step = Step::NotifyCaptureInvalidated;
            } else {
                job.step = Step::ValidateActiveBinding;
            }
            return true;
        }
        if job.step == Step::ValidateActiveBinding {
            if payload["status"] == "pending" {
                job.lifecycle_lookup = true;
                return true;
            }
            let start = record.start.as_ref().expect("active lifecycle start");
            if payload["status"] != "succeeded"
                || payload["target"] != json!(start.lifecycle_target)
                || payload["plan_revision"]
                    != json!(start.lifecycle_candidate.as_ref().unwrap().plan_revision)
                || payload["binding"] != json!(start.lifecycle_candidate)
                || payload["observation"] != json!(record.lifecycle_observation)
            {
                record.lifecycle_invalid_reason = Some("capture_binding_invalid");
                record.plane = None;
                record.input_events.clear();
                job.lifecycle_lookup = false;
                job.step = Step::NotifyCaptureInvalidated;
            } else {
                record.lifecycle_delivery_permit = true;
                record.lifecycle_control_permit = true;
                job.lifecycle_lookup = false;
                return false;
            }
            return true;
        }
        if matches!(
            job.step,
            Step::ValidateLifecycleCandidate
                | Step::AckLifecycleCapture
                | Step::StartLifecycle
                | Step::IssueLifecycleInputBinding
                | Step::AckLifecycleInput
        ) {
            let start = record.start.as_ref().expect("lifecycle start");
            if payload["status"] == "pending" {
                job.lifecycle_lookup = true;
                return true;
            }
            if payload["status"] != "succeeded"
                || payload["target"] != json!(start.lifecycle_target)
                || payload["plan_revision"]
                    != json!(start.lifecycle_candidate.as_ref().unwrap().plan_revision)
            {
                job.fail(record, "lifecycle_authority_rejected", now);
                return true;
            }
            if job.step == Step::StartLifecycle && payload["all_consumers_acknowledged"] != true {
                job.fail(record, "lifecycle_consumers_unconfirmed", now);
                return true;
            }
            job.lifecycle_lookup = false;
            match job.step {
                Step::ValidateLifecycleCandidate => {
                    record.lifecycle_candidate_validated = true;
                    job.step = Step::AcquireOutput;
                }
                Step::AckLifecycleCapture => {
                    job.step = if record.config.stream_audio {
                        Step::Open(Resource::AudioCapture)
                    } else {
                        Step::Activate
                    };
                }
                Step::IssueLifecycleInputBinding => {
                    let Some(token) = payload["token"].as_str().filter(|token| {
                        !token.is_empty()
                            && token.len() <= 512
                            && !token.chars().any(char::is_control)
                    }) else {
                        job.fail(record, "input_binding_unavailable", now);
                        return true;
                    };
                    let Some(revision) = payload["geometry_revision"]
                        .as_u64()
                        .filter(|revision| *revision > 0)
                    else {
                        job.fail(record, "input_binding_unavailable", now);
                        return true;
                    };
                    if revision
                        != start
                            .lifecycle_candidate
                            .as_ref()
                            .unwrap()
                            .geometry_revision
                    {
                        job.fail(record, "input_binding_unavailable", now);
                        return true;
                    }
                    record.lifecycle_input_token = Some(token.to_owned());
                    record.lifecycle_input_geometry_revision = Some(revision);
                    job.step = Step::AcquireInput;
                }
                Step::AckLifecycleInput => {
                    job.step = Step::StartLifecycle;
                }
                Step::StartLifecycle => {
                    record.state = State::Active;
                    record.history.activated(now);
                    record.host_metrics.activated(record.id);
                    record.session_effects.activated(record.id);
                    self.activate_tray_session(record.id);
                    return self.deliver_success(record, job, now, send);
                }
                _ => unreachable!(),
            }
            return true;
        }
        if job.step == Step::ObserveLifecycleCapture {
            if payload["ok"] != true
                || payload["stream"] != record.resources[&Resource::Video]
                || !payload["observed_capture"].is_object()
            {
                job.fail(record, "capture_observation_unavailable", now);
            } else {
                record.lifecycle_observation = Some(payload["observed_capture"].clone());
                job.step = Step::AckLifecycleCapture;
            }
            return true;
        }
        if job.step == Step::ApplyLimiter {
            super::limiter::accept_apply(record, &payload);
            if record.limiter_lease.is_none()
                && matches!(
                    record.limiter_state.as_str(),
                    "not_needed" | "runtime_unavailable" | "lease_conflict" | "policy_only"
                )
            {
                record.limiter_attempted = false;
            }
            job.step = Step::AcquireOutput;
            return true;
        }
        if job.step == Step::CreateVirtualDisplay {
            if payload["ok"] != true {
                record.virtual_display_create_error = payload["code"]
                    .as_str()
                    .or_else(|| payload["error"]["code"].as_str())
                    .map(str::to_owned);
            }
            record.virtual_display_next_poll_at = now;
            job.step = Step::StatusVirtualDisplay;
            return true;
        }
        if job.step == Step::StatusVirtualDisplay {
            match super::virtual_display::accept_status(record, &payload) {
                super::virtual_display::ReferenceStatus::Ready => {
                    if let Some(shared) = self.records.values().find(|other| {
                        other.owns_display_restore
                            && other.display_applied
                            && other.display_lease.is_some()
                            && other.virtual_display_reference_owned
                            && super::virtual_display::same_shared_specification(record, other)
                            && other.state != State::Stopping
                    }) {
                        record.display_applied = true;
                        record.display_state = "applied".into();
                        record.display_status_verified = false;
                        record.owns_display_restore = false;
                        record.display_lease = None;
                        record.display_operation_id = Some(shared.display_operation());
                        record.state = State::Prepared;
                        record.prepared_until = now.saturating_add(u64::from(
                            record
                                .config
                                .display_prepare_timeout_ms
                                .unwrap_or(record.config.ping_timeout_ms),
                        ));
                        return self.deliver_success(record, job, now, send);
                    }
                    job.step = Step::SnapshotDisplay;
                }
                super::virtual_display::ReferenceStatus::Pending => {
                    record.virtual_display_next_poll_at = now.saturating_add(100);
                }
                super::virtual_display::ReferenceStatus::Missing => {
                    let code = super::virtual_display::creation_failure(record);
                    job.fail(record, code, now);
                }
                super::virtual_display::ReferenceStatus::Failed => {
                    let code = super::virtual_display::creation_failure(record);
                    job.fail(record, code, now);
                }
            }
            return true;
        }
        if job.step == Step::SnapshotDisplay {
            if let Err(code) = super::virtual_display::build_display_request(record, &payload) {
                job.fail(record, code, now);
            } else {
                job.step = Step::CheckDisplay;
            }
            return true;
        }
        if job.step == Step::FindLimiter {
            super::limiter::accept_status(record, &payload);
            if record.limiter_lease.is_none()
                && super::limiter::status_confirms_no_lease(record, &payload)
            {
                record.limiter_attempted = false;
            }
            job.step = Step::AcquireOutput;
            return true;
        }
        if job.step == Step::CheckDisplay {
            let read_only = super::virtual_display::is_read_only_policy(record);
            if payload["ok"] == true
                && payload["state"] == "idle"
                && payload["lease"].is_null()
                && payload["native_supported"] == true
                && (payload["mutation_enabled"] == true || read_only)
                && !self
                    .records
                    .values()
                    .any(|other| other.display_attempted || other.display_lease.is_some())
            {
                job.step = Step::ApplyDisplay;
            } else {
                record.display_state = if payload["state"] == "idle" {
                    "unavailable".into()
                } else {
                    "busy".into()
                };
                job.fail(record, "display_unavailable", now);
            }
            return true;
        }
        if job.step == Step::ApplyDisplay {
            if super::virtual_display::read_only_policy_is_satisfied(record, &payload) {
                record.display_attempted = false;
                record.display_status_verified = false;
                // `display_applied` is the stream readiness gate, so read-only
                // Disabled/VerifyOnly policies use its existing satisfied state
                // while deliberately retaining no display restore lease.
                record.display_state = "applied".into();
                record.display_applied = true;
                record.owns_display_restore = false;
                record.display_lease = None;
                record.display_operation_id = None;
                record.state = State::Prepared;
                record.prepared_until = now.saturating_add(u64::from(
                    record
                        .config
                        .display_prepare_timeout_ms
                        .unwrap_or(record.config.ping_timeout_ms),
                ));
                return self.deliver_success(record, job, now, send);
            }
            super::display::accept_apply(record, &payload);
            if payload["ok"] == false && payload["recovery_required"] != true {
                job.fail(record, "display_apply_rejected", now);
            } else {
                job.step = Step::FindDisplay;
            }
            return true;
        }
        if job.step == Step::FindDisplay {
            let known = super::display::accept_status(record, &payload);
            if !known {
                record.display_state = "status_unverified".into();
            }
            if known
                && payload["state"] == "idle"
                && payload["lease"].is_null()
                && record.display_lease.is_none()
                && super::virtual_display::is_read_only_policy(record)
            {
                // The provider stores no journal for a read-only policy. An
                // idle readback confirms no display effect; replaying this
                // verification-only Apply is safe after a lost reply.
                record.display_attempted = false;
                job.step = Step::ApplyDisplay;
                return true;
            }
            if known && record.display_status_verified && record.display_state == "leased" {
                record.display_state = "applied".into();
                record.display_applied = true;
                record.state = State::Prepared;
                record.prepared_until = now.saturating_add(u64::from(
                    record
                        .config
                        .display_prepare_timeout_ms
                        .unwrap_or(record.config.ping_timeout_ms),
                ));
                return self.deliver_success(record, job, now, send);
            }
            job.fail(record, "display_status_unverified", now);
            return true;
        }
        if matches!(
            job.step,
            Step::CreateOwnedVirtualDisplay | Step::FindOwnedVirtualDisplay
        ) {
            if job.step == Step::CreateOwnedVirtualDisplay {
                if payload["ok"] == true {
                    job.step = Step::FindOwnedVirtualDisplay;
                } else if !matches!(
                    payload["error"]["code"].as_str(),
                    Some("startup_reconciliation_pending" | "busy")
                ) {
                    job.fail(record, "virtual_display_create_failed", now);
                }
            } else {
                match super::virtual_display::accept_ready(record, &payload) {
                    Ok(true) => job.step = Step::AcquireOutput,
                    Ok(false) => {}
                    Err(code) => job.fail(record, code, now),
                }
            }
            return true;
        }
        if payload["ok"] != true {
            if job.step == Step::ObserveFeedbackSource {
                retry_feedback_source_observation(record, now);
                return false;
            }
            if matches!(job.step, Step::FeedbackPeek | Step::FeedbackAck) {
                if job.step == Step::FeedbackPeek {
                    record.feedback_ack_through = None;
                }
                record.feedback_poll_at = now.saturating_add(100);
                return false;
            } else if job.step == Step::PersistJournal {
                self.journal.failed = true;
                return self.journal_write_failed(record, job, now, send);
            }
            if job.step == Step::InputBatch
                && record.lifecycle_input_token.is_none()
                && payload["error"]["code"] == "controller_unavailable"
            {
                record.session_effects.vigem_missing(record.id);
                match accept_controller_batch_failure(record, &payload) {
                    Ok(()) => return false,
                    Err(code) => job.fail(record, code, now),
                }
                return true;
            }
            eprintln!(
                "amaterasu session dependency rejected step={:?} code={}",
                job.step,
                safe_dependency_code(&payload),
            );
            job.fail(record, "dependency_rejected", now);
            return true;
        }
        let result = (|| match job.step {
            Step::Open(resource) => {
                let result = self.opened(record, job, resource, &payload, now, send);
                if result.is_ok() && resource == Resource::Video && !record.config.stream_audio {
                    record.plane = Some((operations.factory)(
                        record.handles()?,
                        &record.providers,
                        record.negotiated_route.as_ref(),
                    )?);
                }
                result
            }
            Step::Authenticate => {
                let status = component_status(&payload, record.resources[&Resource::Control]);
                if status.is_some_and(|value| {
                    value["authenticated"] == true && value["state"] == "active"
                }) {
                    record.control_authenticated = true;
                    job.step = Step::MediaReady;
                    Ok(true)
                } else if status.is_some_and(|value| {
                    matches!(
                        value["state"].as_str(),
                        Some("listening" | "connecting" | "awaiting_authentication" | "connected")
                    )
                }) {
                    if record.state == State::Armed {
                        record.next_auth_poll = now.saturating_add(100);
                        return Ok(false);
                    }
                    record.state = State::Prepared;
                    if let Some(request) = job.request {
                        rpc::complete(send, request, error("control_not_authenticated"));
                    }
                    Ok(false)
                } else {
                    Err("control_unavailable")
                }
            }
            Step::MediaReady => {
                let status = component_status(&payload, record.resources[&Resource::Media])
                    .ok_or("invalid_transport_response")?;
                let media = media_status(record, status)?;
                record.media_ready = media.ready;
                record.peer_discovery = media.peer_discovery;
                record.video_peer_port = media.video_port;
                record.audio_peer_port = media.audio_port;
                if media.ready && status["state"] == "sending" && status["failure"].is_null() {
                    if record.state == State::Armed {
                        record.state = State::Starting;
                        job.deadline = now.saturating_add(15000);
                    }
                    if record.config.display_request.is_some()
                        && (record.display_lease.is_none() || !record.display_status_verified)
                    {
                        return Err("display_lease_missing");
                    }
                    job.step = if record
                        .start
                        .as_ref()
                        .is_some_and(|start| start.configuration.extension_profile.is_some())
                    {
                        if record.config.frame_limiter_settings.is_some()
                            && !record.limiter_attempted
                        {
                            Step::ApplyLimiter
                        } else {
                            Step::AcquireOutput
                        }
                    } else {
                        Step::ConfigureMediaVideoFormat
                    };
                    Ok(true)
                } else if status["state"] == "discovering" {
                    if record.state == State::Armed {
                        record.next_auth_poll = now.saturating_add(100);
                    } else {
                        record.state = State::Prepared;
                        if let Some(request) = job.request {
                            rpc::complete(send, request, error("media_not_ready"));
                        }
                    }
                    Ok(false)
                } else {
                    Err("media_unavailable")
                }
            }
            Step::AcquireOutput => {
                require_lease(record, &payload, false)?;
                job.step = Step::Open(Resource::Video);
                Ok(true)
            }
            Step::ConfigureMediaVideoFormat => {
                let start = record.start.as_ref().ok_or("invalid_start_configuration")?;
                if payload["ok"] != true
                    || payload["stream"] != record.resources[&Resource::Media]
                    || payload["video_stream_format"]
                        != json!(start.configuration.video_stream_format)
                {
                    return Err("video_format_configuration_mismatch");
                }
                job.step = if record.config.frame_limiter_settings.is_some()
                    && !record.limiter_attempted
                {
                    Step::ApplyLimiter
                } else {
                    Step::AcquireOutput
                };
                Ok(true)
            }
            Step::AcquireAudio => {
                require_lease(record, &payload, true)?;
                record.plane = Some((operations.factory)(
                    record.handles()?,
                    &record.providers,
                    record.negotiated_route.as_ref(),
                )?);
                job.step = Step::Activate;
                Ok(true)
            }
            Step::Activate => {
                let session =
                    session_response(record, &payload).ok_or("invalid_session_response")?;
                if session["state"] != "active" {
                    return Err("invalid_session_response");
                }
                if record
                    .lifecycle_input_required
                    .unwrap_or(self.settings.enable_input)
                {
                    job.step = if record
                        .start
                        .as_ref()
                        .is_some_and(|start| start.lifecycle_target.is_some())
                    {
                        Step::IssueLifecycleInputBinding
                    } else {
                        Step::AcquireInput
                    };
                    Ok(true)
                } else if record
                    .start
                    .as_ref()
                    .is_some_and(|start| start.lifecycle_target.is_some())
                {
                    job.step = Step::StartLifecycle;
                    Ok(true)
                } else {
                    record.state = State::Active;
                    record.history.activated(now);
                    record.host_metrics.activated(record.id);
                    record.session_effects.activated(record.id);
                    self.activate_tray_session(record.id);
                    Ok(self.deliver_success(record, job, now, send))
                }
            }
            Step::AcquireInput => {
                accept_input_lease(record, &payload, now)?;
                if let Some(token) = record.lifecycle_input_token.as_ref() {
                    let start = record.start.as_ref().ok_or("input_binding_unavailable")?;
                    let epoch = payload["batch_epoch"]
                        .as_str()
                        .filter(|epoch| {
                            !epoch.is_empty()
                                && epoch.len() <= 128
                                && epoch.bytes().all(|b| {
                                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
                                })
                        })
                        .ok_or("input_binding_unavailable")?;
                    if payload["input_binding"] != *token
                        || payload["target"] != json!(start.lifecycle_target)
                        || payload["geometry_revision"]
                            != json!(record.lifecycle_input_geometry_revision)
                        || payload["input_acknowledged"] != true
                    {
                        return Err("input_binding_unavailable");
                    }
                    record.lifecycle_input_epoch = Some(epoch.to_owned());
                }
                if super::virtual_display::requested(record)
                    && record.input_absolute_pointer
                    && record
                        .start
                        .as_ref()
                        .is_some_and(|start| start.mouse != Some(false))
                    && let Some(geometry) = record.capture_geometry
                {
                    let bounds = geometry
                        .normalized_output()
                        .map_err(|_| "invalid_capture_geometry")?;
                    record.input_events.push_front(json!({"kind":"absolute",
                        "x":(u32::from(bounds.left)+u32::from(bounds.right))/2,
                        "y":(u32::from(bounds.top)+u32::from(bounds.bottom))/2}));
                }
                if record
                    .start
                    .as_ref()
                    .is_some_and(|start| start.lifecycle_target.is_some())
                {
                    job.step = Step::AckLifecycleInput;
                    return Ok(true);
                }
                record.state = State::Active;
                record.history.activated(now);
                record.host_metrics.activated(record.id);
                record.session_effects.activated(record.id);
                self.activate_tray_session(record.id);
                Ok(self.deliver_success(record, job, now, send))
            }
            Step::RenewInput => {
                accept_input_renewal(record, &payload, now)?;
                Ok(false)
            }
            Step::InputBatch => {
                if let Some(epoch) = &record.lifecycle_input_epoch
                    && payload["operation_id"]
                        != format!("{epoch}:{}", record.lifecycle_input_sequence)
                {
                    return Err("input_batch_failed");
                }
                accept_input_batch(record, &payload)?;
                if record.lifecycle_input_epoch.is_some() {
                    record.lifecycle_input_sequence = record
                        .lifecycle_input_sequence
                        .checked_add(1)
                        .ok_or("lifecycle_sequence_exhausted")?;
                }
                Ok(false)
            }
            Step::ObserveFeedbackSource => {
                accept_feedback_source_observation(record, &payload)?;
                Ok(false)
            }
            Step::FeedbackPeek => {
                if let Some(through_sequence) = accept_feedback_peek(record, &payload, now)? {
                    record.feedback_ack_through = Some(through_sequence);
                    job.step = Step::FeedbackAck;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Step::FeedbackAck => {
                accept_feedback_ack(record, &payload, now)?;
                Ok(false)
            }
            Step::PollGamepadFeedback => {
                accept_gamepad_feedback(record, &payload, now)?;
                Ok(false)
            }
            Step::ReleaseInput => {
                accept_input_release(record, &payload)?;
                job.step = Step::DrainVideo;
                Ok(true)
            }
            Step::DrainVideo => {
                job.step = if record.resources.contains_key(&Resource::AudioCapture) {
                    Step::CloseCapture
                } else {
                    Step::WaitDrain
                };
                Ok(true)
            }
            Step::CloseCapture => {
                record.resources.remove(&Resource::AudioCapture);
                record.attempted.remove(&Resource::AudioCapture);
                job.step = Step::WaitDrain;
                Ok(true)
            }
            Step::TransportStatus => {
                let status = component_status(&payload, record.resources[&Resource::Media])
                    .ok_or("invalid_transport_response")?;
                match status["state"].as_str() {
                    Some("drained") if status["queued_datagrams"] == 0 => {
                        record.plane = None;
                        job.cleanup = true;
                        job.deadline = now + 15000;
                        Ok(true)
                    }
                    Some("draining") => {
                        job.step = Step::WaitDrain;
                        Ok(true)
                    }
                    _ => Err("transport_drain_failed"),
                }
            }
            Step::ForceIdr => {
                record.force_idr = false;
                Ok(false)
            }
            Step::RecoverAudio => {
                if payload["stream"] != record.resources[&Resource::AudioEncoder]
                    || payload["packet_bytes"] != record.packet_bytes as u64
                {
                    return Err("invalid_audio_recovery_response");
                }
                record.audio_recovery_needed = false;
                if let Some(plane) = record.plane.as_mut() {
                    plane.recover_audio();
                }
                if record.state == State::Stopping {
                    job.step = Step::WaitDrain;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            _ => Err("invalid_workflow"),
        })();
        match result {
            Ok(retain) => retain,
            Err(code) => self.fail_record(record, job, code, now),
        }
    }

    pub(super) fn opened(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        resource: Resource,
        payload: &Value,
        now: u64,
        send: &mut Send<'_>,
    ) -> Result<bool, &'static str> {
        let id = allocation_id(record, resource, payload).ok_or("invalid_component_response")?;
        record.resources.insert(resource, id);
        // An external provider must confirm its native role before the plane
        // can interpret a function table. Retain any allocated handle first so
        // a rejected acknowledgment still follows ordinary owned cleanup.
        record.validate_native_contract(resource, payload)?;
        match resource {
            Resource::Session => {
                if session_response(record, payload)
                    .is_none_or(|value| value["state"] != "preparing")
                {
                    return Err("invalid_session_response");
                }
                if record.config.stream_audio {
                    job.step = Step::Open(Resource::AudioEncoder);
                } else {
                    record.packet_bytes = 0;
                    record.wire_audio_bytes = 16;
                    job.step = Step::Open(Resource::Media);
                }
            }
            Resource::AudioEncoder => {
                let bytes = payload["packet_bytes"]
                    .as_u64()
                    .filter(|bytes| (1..=1275).contains(bytes))
                    .ok_or("unsupported_audio_packet")? as usize;
                if payload["constant_bitrate"] != true
                    || payload["channels"] != record.config.audio.channels
                    || payload["sample_rate"] != 48000
                {
                    return Err("unsupported_audio_format");
                }
                if let Some((streams, coupled, mapping, wave_mapping)) =
                    record.config.audio.channel_mapping()
                {
                    if payload["mapping_family"] != 1
                        || payload["streams"] != streams
                        || payload["coupled_streams"] != coupled
                        || payload["channel_mapping"] != json!(mapping)
                        || payload["wave_channel_mapping"] != json!(wave_mapping)
                        || payload["packet_ms"] != record.config.audio.packet_ms
                        || payload["bitrate"] != record.config.audio.bitrate
                    {
                        return Err("unsupported_audio_format");
                    }
                    record.audio_encoder_layout = Some(json!({
                        "mapping_family": 1,
                        "streams": streams,
                        "coupled_streams": coupled,
                        "channel_mapping": mapping,
                        "wave_channel_mapping": wave_mapping,
                    }));
                }
                record.packet_bytes = bytes;
                record.wire_audio_bytes = if record.config.encrypt_audio {
                    (bytes / 16 + 1) * 16
                } else {
                    bytes
                };
                if record.wire_audio_bytes > 1280 {
                    return Err("unsupported_audio_packet");
                }
                job.step = Step::Open(Resource::Media);
            }
            Resource::Media => {
                record.video_local = Some(local_address(payload, "video_local")?);
                record.audio_local = Some(local_address(payload, "audio_local")?);
                let media = media_status(record, payload)?;
                record.media_ready = media.ready;
                record.peer_discovery = media.peer_discovery;
                record.video_peer_port = media.video_port;
                record.audio_peer_port = media.audio_port;
                job.step = Step::Open(Resource::Control);
            }
            Resource::Control => {
                record.control_local = Some(local_address(payload, "local")?);
                record.retain_control_retirement_epoch(payload);
                if let Some(intent) = &record.config.virtual_display_intent {
                    if intent.active() {
                        if !intent.dd_use_sunshine_virtual_display_driver {
                            record.virtual_display_state = "unsupported_driver".into();
                            job.fail(record, "unsupported_virtual_display_driver", now);
                            return Ok(true);
                        }
                        record.virtual_display_state = "creating".into();
                        job.step = Step::CreateVirtualDisplay;
                        return Ok(true);
                    }
                    // The activation gate and disabled mode both leave explicit
                    // settings in the signed intent but bypass all mutations.
                    record.virtual_display_state = "not_requested".into();
                    record.display_state = "not_requested".into();
                } else if record.config.display_request.is_some() {
                    job.step = Step::CheckDisplay;
                    return Ok(true);
                }
                record.state = State::Prepared;
                record.prepared_until =
                    now.saturating_add(u64::from(record.config.ping_timeout_ms));
                return Ok(self.deliver_success(record, job, now, send));
            }
            Resource::Video => {
                if payload["state"] != "running" || payload["packet_boundaries"] != true {
                    return Err("video_not_running");
                }
                record.accept_negotiated_unit(payload)?;
                configure_absolute_input(record, payload)?;
                job.step = if record
                    .start
                    .as_ref()
                    .is_some_and(|start| start.lifecycle_target.is_some())
                {
                    Step::ObserveLifecycleCapture
                } else if record.config.stream_audio {
                    Step::Open(Resource::AudioCapture)
                } else {
                    Step::Activate
                };
            }
            Resource::AudioCapture => {
                let format = &payload["format"];
                if format["sample_rate"] != 48000
                    || format["channels"] != record.config.audio.channels
                    || !matches!(format["format"].as_str(), Some("f32" | "i16"))
                    || format["channel_mask"] != record.config.audio.channel_mask
                {
                    return Err("unsupported_audio_format");
                }
                record.audio_endpoint = payload["endpoint"]
                    .as_str()
                    .filter(|value| !value.is_empty() && value.len() <= 4096)
                    .map(str::to_owned);
                if record.audio_endpoint.is_none() {
                    return Err("invalid_audio_endpoint");
                }
                job.step = Step::AcquireAudio;
            }
        }
        Ok(true)
    }

    fn deliver_success(
        &self,
        record: &mut Record,
        job: &mut Job,
        now: u64,
        send: &mut Send<'_>,
    ) -> bool {
        if record.state == State::Prepared
            && !record.extension_checked
            && record.extension_route.is_some()
        {
            if record
                .config
                .extension_probe
                .as_ref()
                .is_some_and(|config| config.valid_extension_probe())
            {
                job.step = Step::ProbeExtensionSource;
                return true;
            }
            record.extension_checked = true;
        }
        if job.request.is_none() && record.armed_accepted {
            return false;
        }
        if job.request.is_some_and(|request| {
            rpc::active(send, request) && rpc::complete(send, request, record.response())
        }) {
            false
        } else {
            job.fail(record, "completion_unavailable", now);
            true
        }
    }

    pub(super) fn reply(&self, record: &Record, job: &Job, send: &mut Send<'_>) {
        if let Some(request) = job.request {
            rpc::complete(send, request, record.response());
        }
    }

    fn fail_record(
        &self,
        record: &mut Record,
        job: &mut Job,
        code: &'static str,
        now: u64,
    ) -> bool {
        job.fail(record, code, now);
        true
    }

    fn transfer_shared_display_restore(&mut self, record: &mut Record) -> bool {
        if !record.owns_display_restore
            || !record.display_applied
            || record.display_lease.is_none()
            || !record.virtual_display_reference_owned
        {
            return false;
        }
        let Some(shared) = self.records.values_mut().find(|candidate| {
            candidate.state != State::Stopping
                && candidate.state != State::CleanupFailed
                && candidate.virtual_display_reference_owned
                && candidate.display_applied
                && super::virtual_display::same_shared_specification(record, candidate)
        }) else {
            return false;
        };
        shared.display_attempted = true;
        shared.display_lease = record.display_lease.take();
        shared.display_status_verified = record.display_status_verified;
        shared.display_state = record.display_state.clone();
        shared.display_applied = true;
        shared.owns_display_restore = true;
        shared.display_operation_id = Some(record.display_operation());

        record.display_attempted = false;
        record.display_status_verified = false;
        record.display_state = "shared".into();
        record.display_applied = false;
        record.owns_display_restore = false;
        record.display_operation_id = None;
        true
    }

    fn journal_write_failed(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        now: u64,
        send: &mut Send<'_>,
    ) -> bool {
        record.cleanup_error = Some("lifecycle_journal_write_failed");
        if job.cleanup {
            record.state = State::CleanupFailed;
            self.reply(record, job, send);
            return false;
        }
        job.fail(record, "lifecycle_journal_write_failed", now);
        true
    }
}

fn safe_dependency_code(payload: &serde_json::Value) -> &str {
    payload["code"]
        .as_str()
        .or_else(|| payload["error"]["code"].as_str())
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 64
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
        .unwrap_or("unavailable")
}

#[cfg(test)]
mod diagnostic_code_tests {
    use super::safe_dependency_code;
    use serde_json::json;

    #[test]
    fn dependency_code_trace_accepts_only_bounded_status_tokens() {
        assert_eq!(
            safe_dependency_code(&json!({"code":"backend_unavailable"})),
            "backend_unavailable"
        );
        assert_eq!(
            safe_dependency_code(&json!({"error":{"code":"codec_unavailable"}})),
            "codec_unavailable"
        );
        assert_eq!(
            safe_dependency_code(&json!({"code":"key_hex=secret"})),
            "unavailable"
        );
        assert_eq!(
            safe_dependency_code(&json!({"code":"a".repeat(65)})),
            "unavailable"
        );
    }
}
