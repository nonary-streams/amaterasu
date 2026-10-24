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
                }
                record.cleanup_error = Some("cleanup_deadline");
                record.state = State::CleanupFailed;
                self.reply(record, job, send);
                return false;
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
                        }
                        record.limiter_state = "apply_uncertain".into();
                        job.fail(record, "frame_limiter_apply_uncertain", now);
                        return true;
                    }
                    if job.step == Step::ApplyDisplay {
                        let settled = pending.shutdown(send);
                        if !settled {
                            record.uncertain = Some(pending);
                        }
                        record.display_state = "apply_uncertain".into();
                        job.fail(record, "display_apply_uncertain", now);
                        return true;
                    }
                    record.uncertain = Some(pending);
                    record.plane = None;
                    record.audio_recovery_needed = false;
                    record.cleanup_error = Some("request_cancellation_unconfirmed");
                    record.state = State::CleanupFailed;
                    self.reply(record, job, send);
                    return false;
                }
                rpc::Progress::Result(payload) => {
                    return self.consume(record, job, payload, now, send, factory);
                }
                rpc::Progress::Failed => {
                    if job.step == Step::PersistJournal {
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
                                | Step::FindDisplay
                                | Step::RestoreDisplay
                                | Step::RecoverDisplay
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
        if job.cleanup && job.step != Step::PersistJournal {
            let Some(step) = job.next_cleanup(record) else {
                return self.finish_cleanup(record, job, send);
            };
            job.step = step;
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
        if let Step::Open(resource) = job.step {
            record.attempted.insert(resource);
        }
        if job.step == Step::ApplyLimiter {
            record.limiter_attempted = true;
        }
        if job.step == Step::ApplyDisplay {
            record.display_attempted = true;
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
        if !job.cleanup && job.step != Step::PersistJournal && !self.journal.matches_record(record)
        {
            let document = match self.journal.with_record(record) {
                Ok(document) => document,
                Err(code) => {
                    job.fail(record, code, now);
                    return true;
                }
            };
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
        factory: &mut Factory<'_>,
    ) -> bool {
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
            if payload["ok"] == true
                && payload["state"] == "idle"
                && payload["lease"].is_null()
                && payload["native_supported"] == true
                && payload["mutation_enabled"] == true
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
            if known && record.display_status_verified && record.display_state == "leased" {
                record.display_state = "applied".into();
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
        if payload["ok"] != true {
            if job.step == Step::PersistJournal {
                self.journal.failed = true;
                return self.journal_write_failed(record, job, now, send);
            }
            if job.step == Step::InputBatch && payload["error"]["code"] == "controller_unavailable"
            {
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
                    record.plane = Some(factory(record.handles()?)?);
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
                    job.step = if record.config.frame_limiter_settings.is_some()
                        && !record.limiter_attempted
                    {
                        Step::ApplyLimiter
                    } else {
                        Step::AcquireOutput
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
            Step::AcquireAudio => {
                require_lease(record, &payload, true)?;
                record.plane = Some(factory(record.handles()?)?);
                job.step = Step::Activate;
                Ok(true)
            }
            Step::Activate => {
                let session =
                    session_response(record, &payload).ok_or("invalid_session_response")?;
                if session["state"] != "active" {
                    return Err("invalid_session_response");
                }
                if self.settings.enable_input {
                    job.step = Step::AcquireInput;
                    Ok(true)
                } else {
                    record.state = State::Active;
                    record.history.activated(now);
                    Ok(self.deliver_success(record, job, now, send))
                }
            }
            Step::AcquireInput => {
                accept_input_lease(record, &payload, now)?;
                record.state = State::Active;
                record.history.activated(now);
                Ok(self.deliver_success(record, job, now, send))
            }
            Step::RenewInput => {
                accept_input_renewal(record, &payload, now)?;
                Ok(false)
            }
            Step::InputBatch => {
                accept_input_batch(record, &payload)?;
                Ok(false)
            }
            Step::ReleaseInput => {
                accept_input_release(record, &payload)?;
                job.step = Step::DrainVideo;
                Ok(true)
            }
            Step::DrainVideo => {
                job.step = Step::CloseCapture;
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

    fn opened(
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
                if record.config.display_request.is_some() {
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
                job.step = if record.config.stream_audio {
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
