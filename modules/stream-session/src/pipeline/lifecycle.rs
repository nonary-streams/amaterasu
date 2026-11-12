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
        factory: &mut Factory<'_>,
    ) -> bool {
        let shutdown_deadline = self.shutdown_deadline.or_else(|| {
            (job.cleanup || record.state == State::Stopping)
                .then_some(self.shutdown_announced_deadline)
                .flatten()
        });
        if let Some(deadline) = shutdown_deadline {
            job.deadline = job.deadline.min(deadline);
        }
        if now >= job.deadline && job.step == Step::PersistJournal {
            if job.pending.is_none() {
                // No broker request was issued, so there is no late write to
                // fence. Retire the expired workflow without claiming one.
                if job.cleanup {
                    record.cleanup_error = Some("cleanup_deadline");
                    record.state = State::CleanupFailed;
                    self.reply(record, job, send);
                    return false;
                }
                job.step = job.journal_resume.take().unwrap_or(Step::WaitDrain);
                job.journal_document = None;
                job.journal_payload = None;
                job.journal_operation_id = None;
                job.fail(record, "operation_deadline", now);
                return true;
            }
            // Ownership persistence may already have committed. Retain its
            // exact broker identity for reconciliation; expiry must
            // not cancel the write or continue into unjournaled effects.
            if let Some(pending) = job.pending.take() {
                self.retain_uncertain_journal(record, job, pending, now);
            }
            self.journal.failed = true;
            self.recovery_blocked = true;
            record.cleanup_error = Some("lifecycle_journal_write_unconfirmed");
            record.state = State::CleanupFailed;
            self.reply(record, job, send);
            // Keep this separately from any uncertain dependent operation,
            // while releasing the workflow so status/stop remain available.
            return false;
        }
        if let Some(deadline) = self.shutdown_deadline
            && now >= job.deadline
            && !job.cleanup
            && record.state == State::Active
            && job.step == Step::ObserveActiveBinding
        {
            // This is only the Video provider's caller-owned status read. It
            // cannot allocate, extend a lease or mutate lifecycle state. Keep
            // its exact settlement separate so healthy native teardown proceeds.
            if let Some(mut pending) = job.pending.take() {
                pending.cancel();
                record.shutdown_observations.push(pending);
            }
            *job = super::actions::stop_job(
                record,
                job.request,
                now,
                self.settings.drain_timeout_ms,
                false,
            );
            job.deadline = job.deadline.min(deadline);
        }
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
                record.input_timing.clear_queue();
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
                    if job.step == Step::PersistJournal {
                        self.retain_uncertain_journal(record, job, pending, now);
                        self.journal.failed = true;
                        self.recovery_blocked = true;
                        record.cleanup_error = Some("lifecycle_journal_write_unconfirmed");
                        record.state = State::CleanupFailed;
                        self.reply(record, job, send);
                        return false;
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
                    if job.step == Step::PersistJournal {
                        if !job.journal_committed {
                            let saved = job.journal_document.as_ref().is_some_and(|document| {
                                self.journal
                                    .validate_saved_response(&payload, document.clone())
                                    .is_ok()
                            });
                            if !saved {
                                self.journal.failed = true;
                                return self.journal_write_failed(record, job, now, send);
                            }
                            job.journal_committed = true;
                        }
                        if !pending.release_result(send) {
                            job.pending = Some(pending);
                            return true;
                        }
                    }
                    if matches!(
                        job.step,
                        Step::ValidateLifecycleCandidate
                            | Step::ReadFrozenProcessingSettings
                            | Step::PlanGpuProcessing
                            | Step::ObserveLifecycleCapture
                            | Step::AckLifecycleCapture
                            | Step::AckLifecycleStream
                            | Step::StartLifecycle
                            | Step::PauseLifecycle
                            | Step::RetireLifecycle
                            | Step::RecoverLifecycle
                            | Step::ResolveLifecyclePreparation
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
                    return self.consume(record, job, payload, now, send, factory);
                }
                rpc::Progress::Failed => {
                    if matches!(
                        job.step,
                        Step::ObserveActiveBinding | Step::ValidateActiveBinding
                    ) {
                        record.plane = None;
                        record.input_events.clear();
                        record.input_timing.clear_queue();
                        record.lifecycle_invalid_reason = Some("capture_validation_unavailable");
                        job.lifecycle_lookup = false;
                        job.step = Step::NotifyCaptureInvalidated;
                        return true;
                    }
                    if matches!(job.step, Step::FeedbackPeek | Step::FeedbackAck) {
                        if job.step == Step::FeedbackPeek {
                            record.feedback_ack_through = None;
                        }
                        record.feedback_poll_at = now.saturating_add(100);
                        return false;
                    } else if job.step == Step::PersistJournal {
                        if !pending.admission_rejected() {
                            // A lost admission may have delivered this write even
                            // if a later retry failed. Keep its identity fenced.
                            self.retain_uncertain_journal(record, job, pending, now);
                            self.journal.failed = true;
                            self.recovery_blocked = true;
                            record.cleanup_error = Some("lifecycle_journal_write_unconfirmed");
                            record.state = State::CleanupFailed;
                            self.reply(record, job, send);
                            return false;
                        }
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
                        if matches!(
                            job.step,
                            Step::PauseLifecycle | Step::RetireLifecycle | Step::RecoverLifecycle
                        ) {
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
                                | Step::FindOwnedVirtualDisplay
                                | Step::RemoveOwnedVirtualDisplay
                                | Step::ResolveLifecyclePreparation
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
                            job.step = Step::AcquireOutput;
                        }
                        return true;
                    }
                    if job.step == Step::FindDisplay {
                        record.display_state = "status_unavailable".into();
                        if record.state == State::Preparing {
                            job.fail(record, "display_status_unverified", now);
                            return true;
                        }
                        job.step = Step::AcquireOutput;
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
            super::shared_capture::detach_if_borrowed(record, &self.records);
            transferred_display_restore = match self.transfer_shared_display_restore(record) {
                Ok(transferred) => transferred,
                Err(()) => {
                    self.journal_waiting_stops.insert(record.id);
                    record.cleanup_error = Some("lifecycle_journal_write_unconfirmed");
                    record.state = State::CleanupFailed;
                    self.reply(record, job, send);
                    return false;
                }
            };
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
                .is_some_and(|start| start.manages_lifecycle_capture())
            && !record.lifecycle_candidate_validated
        {
            job.step = Step::ValidateLifecycleCandidate;
        }
        if job.step == Step::AcquireOutput
            && record.processing_plan_provider.is_some()
            && !record.processing_plan_complete
        {
            job.step = Step::ReadFrozenProcessingSettings;
        }
        if job.step == Step::AcquireOutput
            && let Err(code) = super::shared_capture::initialize_source(record)
        {
            job.fail(record, code, now);
            return true;
        }
        if job.step == Step::AcquireOutput
            && super::virtual_display::requested(record)
            && record.virtual_display_output.is_none()
        {
            job.step = if record.config.shared_source_operation_id.is_some() {
                Step::FindOwnedVirtualDisplay
            } else {
                Step::CreateOwnedVirtualDisplay
            };
        }
        if let Err(code) = super::source_preparation::redirect(record, job) {
            job.fail(record, code, now);
            return true;
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
            record
                .input_timing
                .begin_batch(record.input_events.len(), record.input_inflight);
        }
        if job.step == Step::FeedbackPeek {
            record.feedback_peek_generation = record.contribution_barrier.ready_generation();
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
        // An unconfirmed write anywhere in the pipeline owns the current
        // revision. Never dispatch a later write until that fence is resolved.
        if job.step == Step::PersistJournal && self.journal.failed {
            if job.cleanup {
                self.journal_waiting_stops.insert(record.id);
            }
            record.cleanup_error = Some("lifecycle_journal_write_unconfirmed");
            record.state = State::CleanupFailed;
            self.reply(record, job, send);
            return false;
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
        let pending = if matches!(
            job.step,
            Step::OpenSourceScope | Step::PrepareSourceHook | Step::CloseSourceScope
        ) {
            rpc::Pending::versioned_api(&service, payload, operation_id, parent)
        } else if job.step == Step::PlanGpuProcessing {
            rpc::Pending::processing_api(
                &service,
                record
                    .processing_plan_provider
                    .as_ref()
                    .expect("processing provider")
                    .interface_minor,
                payload,
                operation_id,
                parent,
            )
        } else {
            rpc::Pending::new(&service, payload, operation_id, parent)
        };
        job.pending = Some(
            if matches!(job.step, Step::RecoverAudio | Step::PersistJournal) {
                // Reuse the exact broker operation ID after a lost admission and
                // never cancel a possibly delivered ownership write.
                pending.retry_uncertain_admission()
            } else {
                pending
            },
        );
        true
    }

    fn retain_uncertain_journal(
        &self,
        record: &mut Record,
        job: &mut Job,
        pending: rpc::Pending,
        now: u64,
    ) {
        record.journal_uncertain = Some(super::record::UncertainJournalWrite {
            pending,
            document: job.journal_document.take(),
            validated: job.journal_committed,
            next_poll: now.saturating_add(100),
        });
    }

    fn consume(
        &mut self,
        record: &mut Record,
        job: &mut Job,
        payload: Value,
        now: u64,
        send: &mut Send<'_>,
        factory: &mut Factory<'_>,
    ) -> bool {
        if job.step == Step::PersistJournal {
            debug_assert!(job.journal_committed);
            job.journal_document = None;
            job.journal_committed = false;
            job.journal_payload = None;
            job.journal_operation_id = None;
            job.step = job.journal_resume.take().unwrap_or(Step::WaitDrain);
            if job.cleanup {
                job.settle_uncertain(record);
            }
            return true;
        }
        if super::source_preparation::consume(record, job, &payload, now) {
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
                record.input_timing.clear_queue();
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
                record.input_timing.clear_queue();
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
        if job.step == Step::ReadFrozenProcessingSettings {
            let start = record.start.as_ref().expect("processing requires start");
            let plan = record
                .processing_plan_provider
                .as_ref()
                .expect("processing provider");
            let valid = payload["status"] == "succeeded"
                && payload["target"] == json!(start.lifecycle_target)
                && payload["provider"] == plan.provider
                && payload["revision"]
                    .as_str()
                    .is_some_and(|revision| !revision.is_empty() && revision.len() <= 128)
                && payload["settings"].is_object()
                && serde_json::to_vec(&payload["settings"])
                    .is_ok_and(|bytes| bytes.len() <= 16_384);
            if !valid {
                job.fail(record, "processing_settings_unavailable", now);
                return true;
            }
            record.processing_frozen = Some((
                payload["revision"].as_str().unwrap().to_owned(),
                payload["settings"].clone(),
            ));
            job.step = Step::PlanGpuProcessing;
            return true;
        }
        if job.step == Step::PlanGpuProcessing {
            let start = record.start.as_ref().expect("processing requires start");
            let plan = record
                .processing_plan_provider
                .as_ref()
                .expect("processing provider");
            let (revision, frozen) = record.processing_frozen.as_ref().expect("frozen settings");
            let enabled = payload["enabled"].as_bool();
            let source = payload["source_hdr_required"].as_bool();
            let output = payload["output_hdr_required"].as_bool();
            let actual_source = start
                .lifecycle_candidate
                .as_ref()
                .map(|candidate| candidate.mode.hdr);
            let actual_output = start.configuration.video_stream_format.dynamic_range
                == amaterasu_module_sdk::DynamicRange::Hdr;
            let valid = payload["status"] == "succeeded"
                && payload["target"] == json!(start.lifecycle_target)
                && payload["revision"] == *revision
                && payload["processor_service"] == plan.processor_service
                && amaterasu_module_sdk::extension_api::validate_processing_configuration(
                    frozen,
                    &payload["processor_settings"],
                )
                .is_ok()
                && enabled.is_some()
                && (payload["source_hdr_required"].is_null() || source.is_some())
                && (payload["output_hdr_required"].is_null() || output.is_some())
                // A selected transform must declare both sides of its range contract.
                && (enabled != Some(true) || (source.is_some() && output.is_some()))
                // Managed candidates are authenticated before this point. External
                // capture proves its source range at the video frame boundary.
                && source.is_none_or(|required| actual_source.is_none_or(|actual| actual == required))
                && output.is_none_or(|required| actual_output == required)
                // Bypassing the processor cannot change the source dynamic range.
                && (enabled != Some(false) || actual_source.is_none_or(|actual| actual == actual_output));
            if !valid {
                job.fail(record, "processing_plan_rejected", now);
                return true;
            }
            let required_source = if enabled == Some(false) {
                actual_output
            } else {
                source.unwrap()
            };
            // An SDR stream with processing disabled may use the existing
            // video path only when trusted composition selects opt-in behavior.
            // HDR output retains frame provenance checks even on bypass.
            if enabled != Some(false) || actual_output || !plan.omit_disabled {
                let mut processing = json!({"service":plan.processor_service,
                    "enabled":enabled.unwrap(), "settings":payload["processor_settings"],
                    "source_hdr_required":required_source});
                if plan.live_settings {
                    processing["live_settings"] = json!({"target":start.lifecycle_target,
                        "provider":plan.provider,"revision":revision});
                }
                record.gpu_processing = Some(processing);
            }
            record.processing_plan_complete = true;
            job.step = Step::AcquireOutput;
            return true;
        }
        if matches!(
            job.step,
            Step::ValidateLifecycleCandidate
                | Step::AckLifecycleCapture
                | Step::AckLifecycleStream
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
                    != json!(
                        start
                            .lifecycle_candidate
                            .as_ref()
                            .map(|candidate| candidate.plan_revision)
                    )
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
                    job.step = if record.processing_plan_provider.is_some() {
                        Step::ReadFrozenProcessingSettings
                    } else {
                        Step::AcquireOutput
                    };
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
                    let expected_revision = if start.lifecycle_capture_mode.is_external() {
                        match &start.lifecycle_target {
                            Some(amaterasu_extension_api::LifecycleTarget::Stream {
                                generation,
                                ..
                            }) => *generation,
                            _ => 0,
                        }
                    } else {
                        start
                            .lifecycle_candidate
                            .as_ref()
                            .map_or(0, |candidate| candidate.geometry_revision)
                    };
                    if revision != expected_revision {
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
                Step::AckLifecycleStream => {
                    job.step = if record.lifecycle_input_required == Some(true) {
                        Step::IssueLifecycleInputBinding
                    } else {
                        Step::StartLifecycle
                    };
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
            if matches!(job.step, Step::ProbeAudioReconfiguration(_)) {
                record.pending_audio = None;
                if let Some(request) = job.request {
                    rpc::complete(send, request, error("unsupported_audio_format"));
                }
                return false;
            }
            if matches!(job.step, Step::FeedbackPeek | Step::FeedbackAck) {
                if job.step == Step::FeedbackPeek {
                    record.feedback_ack_through = None;
                    if record
                        .start
                        .as_ref()
                        .is_some_and(|start| start.outbound_agreement.is_some())
                        && matches!(
                            payload["error"]["code"].as_str(),
                            Some("invalid_request" | "unsupported")
                        )
                    {
                        record.feedback_contributions_unsupported = true;
                    }
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
                    record.plane = Some(factory(record.handles()?, &record.providers)?);
                }
                result
            }
            Step::ProbeAudio(resource) => {
                let probe: amaterasu_module_sdk::audio::AudioProbeResult =
                    serde_json::from_value(payload.clone()).map_err(|_| "invalid_audio_probe")?;
                if !probe.ok
                    || !probe.supported
                    || serde_json::to_value(probe.audio_profile)
                        .map_err(|_| "invalid_audio_probe")?
                        != record.config.audio.custom_profile()
                {
                    return Err("unsupported_audio_format");
                }
                job.step = match resource {
                    Resource::AudioCapture if record.config.audio.encoded_source() => {
                        Step::ProbeAudio(Resource::Media)
                    }
                    Resource::AudioCapture
                        if record
                            .config
                            .audio
                            .custom
                            .as_ref()
                            .is_some_and(|custom| custom.processing.is_some()) =>
                    {
                        Step::ProbeAudio(Resource::AudioProcessor)
                    }
                    Resource::AudioCapture | Resource::AudioProcessor => {
                        Step::ProbeAudio(Resource::AudioEncoder)
                    }
                    Resource::AudioEncoder => Step::ProbeAudio(Resource::Media),
                    Resource::Media if record.config.audio.encoded_source() => {
                        record.packet_bytes = 0;
                        record.wire_audio_bytes = 0;
                        Step::Open(Resource::Media)
                    }
                    Resource::Media
                        if record
                            .config
                            .audio
                            .custom
                            .as_ref()
                            .is_some_and(|custom| custom.processing.is_some()) =>
                    {
                        Step::Open(Resource::AudioProcessor)
                    }
                    Resource::Media => Step::Open(Resource::AudioEncoder),
                    _ => return Err("invalid_audio_probe"),
                };
                Ok(true)
            }
            Step::ProbeAudioReconfiguration(resource) => {
                let candidate = record
                    .pending_audio
                    .as_ref()
                    .ok_or("invalid_audio_reconfiguration")?;
                let supported = serde_json::from_value::<
                    amaterasu_module_sdk::audio::AudioProbeResult,
                >(payload.clone())
                .is_ok_and(|probe| {
                    probe.ok
                        && probe.supported
                        && serde_json::to_value(probe.audio_profile)
                            .is_ok_and(|profile| profile == candidate.custom_profile())
                });
                if !supported {
                    record.pending_audio = None;
                    if let Some(request) = job.request {
                        rpc::complete(send, request, error("unsupported_audio_format"));
                    }
                    return Ok(false);
                }
                job.step = match resource {
                    Resource::AudioCapture if candidate.encoded_source() => {
                        Step::ProbeAudioReconfiguration(Resource::Media)
                    }
                    Resource::AudioCapture
                        if candidate
                            .custom
                            .as_ref()
                            .is_some_and(|custom| custom.processing.is_some()) =>
                    {
                        Step::ProbeAudioReconfiguration(Resource::AudioProcessor)
                    }
                    Resource::AudioCapture | Resource::AudioProcessor => {
                        Step::ProbeAudioReconfiguration(Resource::AudioEncoder)
                    }
                    Resource::AudioEncoder => Step::ProbeAudioReconfiguration(Resource::Media),
                    Resource::Media if candidate.encoded_source() => {
                        Step::ApplyAudioReconfiguration(Resource::Media)
                    }
                    Resource::Media
                        if candidate
                            .custom
                            .as_ref()
                            .is_some_and(|custom| custom.processing.is_some()) =>
                    {
                        Step::ApplyAudioReconfiguration(Resource::AudioProcessor)
                    }
                    Resource::Media => Step::ApplyAudioReconfiguration(Resource::AudioEncoder),
                    _ => return Err("invalid_audio_probe"),
                };
                Ok(true)
            }
            Step::ApplyAudioReconfiguration(resource) => {
                let candidate = record
                    .pending_audio
                    .as_ref()
                    .ok_or("invalid_audio_reconfiguration")?;
                let reply: amaterasu_module_sdk::audio::AudioOpenResult =
                    serde_json::from_value(payload.clone())
                        .map_err(|_| "invalid_audio_reconfiguration_response")?;
                if !reply.ok
                    || reply.stream != record.resources[&resource]
                    || serde_json::to_value(reply.audio_profile)
                        .map_err(|_| "invalid_audio_reconfiguration_response")?
                        != candidate.custom_profile()
                {
                    return Err("invalid_audio_reconfiguration_response");
                }
                if resource == Resource::Media
                    && [
                        ("video_local", &record.video_local),
                        ("audio_local", &record.audio_local),
                    ]
                    .into_iter()
                    .any(|(name, original)| {
                        reply
                            .details
                            .get(name)
                            .is_some_and(|value| value.as_str() != original.as_deref())
                    })
                {
                    return Err("audio_reconfiguration_changed_transport");
                }
                job.step = match resource {
                    Resource::AudioProcessor => {
                        Step::ApplyAudioReconfiguration(Resource::AudioEncoder)
                    }
                    Resource::AudioEncoder => Step::ApplyAudioReconfiguration(Resource::Media),
                    Resource::Media => {
                        record.config.audio = record
                            .pending_audio
                            .take()
                            .expect("pending audio configuration");
                        record.packet_bytes = 0;
                        record.wire_audio_bytes = 0;
                        if let Some(request) = job.request {
                            rpc::complete(send, request, record.response());
                        }
                        return Ok(false);
                    }
                    _ => return Err("invalid_audio_reconfiguration"),
                };
                Ok(true)
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
                        && !(super::virtual_display::is_read_only_policy(record)
                            && record.display_applied
                            && record.display_state == "applied")
                    {
                        return Err("display_lease_missing");
                    }
                    job.step = Step::ConfigureMediaVideoFormat;
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
                job.step = Step::AcquireOutput;
                Ok(true)
            }
            Step::AcquireAudio => {
                require_lease(record, &payload, true)?;
                record.plane = Some(factory(record.handles()?, &record.providers)?);
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
                    job.step = match record.start.as_ref() {
                        Some(start) if start.manages_lifecycle_capture() => {
                            Step::IssueLifecycleInputBinding
                        }
                        Some(start) if start.lifecycle_target.is_some() => Step::AckLifecycleStream,
                        _ => Step::AcquireInput,
                    };
                    Ok(true)
                } else if record
                    .start
                    .as_ref()
                    .is_some_and(|start| start.lifecycle_target.is_some())
                {
                    job.step = if record
                        .start
                        .as_ref()
                        .is_some_and(|start| start.lifecycle_capture_mode.is_external())
                    {
                        Step::AckLifecycleStream
                    } else {
                        Step::StartLifecycle
                    };
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
                    record.input_timing.queued_front(record.input_events.len());
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
                accept_acknowledged_input_batch(record, &payload)?;
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
                job.step = if record.resources.contains_key(&Resource::AudioCapture)
                    && !record.config.audio.encoded_source()
                {
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
                    || (record.config.audio.custom.is_none()
                        && payload["packet_bytes"] != record.packet_bytes as u64)
                    || (record.config.audio.custom.is_some()
                        && payload["audio_profile"] != record.config.audio.custom_profile())
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
                if record.config.stream_audio && record.config.audio.custom.is_some() {
                    job.step = Step::ProbeAudio(Resource::AudioCapture);
                } else if record.config.stream_audio && !record.config.audio.encoded_source() {
                    job.step = Step::Open(
                        if record
                            .config
                            .audio
                            .custom
                            .as_ref()
                            .is_some_and(|custom| custom.processing.is_some())
                        {
                            Resource::AudioProcessor
                        } else {
                            Resource::AudioEncoder
                        },
                    );
                } else {
                    record.packet_bytes = 0;
                    record.wire_audio_bytes = if record.config.audio.encoded_source() {
                        0
                    } else {
                        16
                    };
                    job.step = Step::Open(Resource::Media);
                }
            }
            Resource::AudioProcessor => {
                if payload["audio_profile"] != record.config.audio.custom_profile() {
                    return Err("unsupported_audio_processing_format");
                }
                job.step = Step::Open(Resource::AudioEncoder);
            }
            Resource::AudioEncoder => {
                if let Some(custom) = &record.config.audio.custom {
                    if payload["audio_profile"] != record.config.audio.custom_profile()
                        || payload["channels"] != record.config.audio.channels
                        || payload["sample_rate"] != custom.sample_rate
                    {
                        return Err("unsupported_audio_format");
                    }
                    record.packet_bytes = match payload.get("packet_bytes") {
                        None | Some(Value::Null) => 0,
                        Some(value) => value
                            .as_u64()
                            .filter(|bytes| {
                                *bytes <= amaterasu_media::MAX_ENCODED_AUDIO_BYTES as u64
                            })
                            .ok_or("unsupported_audio_packet")?
                            as usize,
                    };
                    // The selected custom transport owns framing and encryption.
                    record.wire_audio_bytes = 0;
                    job.step = Step::Open(Resource::Media);
                    return Ok(true);
                }
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
                configure_absolute_input(record, payload)?;
                job.step = if record
                    .start
                    .as_ref()
                    .is_some_and(|start| start.manages_lifecycle_capture())
                {
                    Step::ObserveLifecycleCapture
                } else if record.config.stream_audio {
                    Step::Open(Resource::AudioCapture)
                } else {
                    Step::Activate
                };
            }
            Resource::AudioCapture => {
                let start = record
                    .start
                    .as_ref()
                    .expect("validated audio capture start");
                if start.virtual_sink.is_none()
                    && start.channels.is_some()
                    && (payload.get("virtual_sink") != Some(&Value::Null)
                        || payload.get("channels") != Some(&Value::Null)
                        || payload["capture_sink_without_routing"] != true
                        || payload["host_audio_enabled"] != start.host_audio_enabled
                        || payload["requested_endpoint"] != start.audio_endpoint)
                {
                    return Err("audio_capture_policy_unverified");
                }
                let format = &payload["format"];
                let sample_rate = record
                    .config
                    .audio
                    .custom
                    .as_ref()
                    .map_or(48_000, |custom| {
                        custom
                            .pcm
                            .as_ref()
                            .map_or(custom.sample_rate, |pcm| pcm.sample_rate)
                    });
                let input_channels = record
                    .config
                    .audio
                    .custom
                    .as_ref()
                    .and_then(|custom| custom.pcm.as_ref())
                    .map_or(record.config.audio.channels as usize, |pcm| {
                        pcm.speakers.len()
                    });
                if format["sample_rate"] != sample_rate
                    || format["channels"] != input_channels
                    || if let Some(custom) = &record.config.audio.custom {
                        payload["audio_profile"] != record.config.audio.custom_profile()
                            || if let Some(pcm) = &custom.pcm {
                                payload["pcm_format"]
                                    != serde_json::to_value(pcm)
                                        .map_err(|_| "unsupported_audio_format")?
                            } else {
                                payload["audio_source"] != "encoded"
                            }
                    } else {
                        !matches!(format["format"].as_str(), Some("f32" | "i16"))
                            || format["channel_mask"] != record.config.audio.channel_mask
                    }
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

    fn transfer_shared_display_restore(&mut self, record: &mut Record) -> Result<bool, ()> {
        if !record.owns_display_restore
            || !record.display_applied
            || record.display_lease.is_none()
            || !record.virtual_display_reference_owned
        {
            return Ok(false);
        }
        let Some(shared) = self.records.values_mut().find(|candidate| {
            candidate.state != State::Stopping
                && candidate.state != State::CleanupFailed
                && candidate.virtual_display_reference_owned
                && candidate.display_applied
                && super::virtual_display::same_shared_specification(record, candidate)
        }) else {
            return Ok(false);
        };
        // Moving ownership in memory requires a durable update for both
        // records before the original reference may be released.
        if self.journal.failed {
            return Err(());
        }
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
        Ok(true)
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
