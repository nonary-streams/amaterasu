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
                    record.uncertain = Some(pending);
                    record.plane = None;
                    record.cleanup_error = Some("request_cancellation_unconfirmed");
                    record.state = State::CleanupFailed;
                    self.reply(record, job, send);
                    return false;
                }
                rpc::Progress::Result(payload) => {
                    return self.consume(record, job, payload, now, send, factory);
                }
                rpc::Progress::Failed => {
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
                        ) {
                            self.cleanup_rejected(record, job, send)
                        } else {
                            true
                        };
                    }
                    job.fail(record, "dependency_unavailable", now);
                }
            }
        }
        if job.cleanup {
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
            record.input_inflight = record.input_events.len().min(MAX_BATCH_EVENTS);
        }
        let (service, payload) = job.command(record);
        self.sequence = match self.sequence.checked_add(1) {
            Some(value) => value,
            None => {
                job.fail(record, "operation_counter_exhausted", now);
                return true;
            }
        };
        let parent = if job.cleanup || record.state == State::Stopping {
            None
        } else {
            job.request
        };
        job.pending = Some(rpc::Pending::new(
            &service,
            payload,
            format!("ss-{:x}-{}", record.id, self.sequence),
            parent,
        ));
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
        if job.cleanup {
            return self.consume_cleanup(record, job, payload, send);
        }
        if payload["ok"] != true {
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
                    job.step = Step::AcquireOutput;
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
                    Ok(self.deliver_success(record, job, now, send))
                }
            }
            Step::AcquireInput => {
                accept_input_lease(record, &payload, now)?;
                record.state = State::Active;
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
                    || payload["channels"] != 2
                    || payload["sample_rate"] != 48000
                {
                    return Err("unsupported_audio_format");
                }
                record.packet_bytes = bytes;
                record.wire_audio_bytes = if record.config.encrypt_audio {
                    (bytes / 16 + 1) * 16
                } else {
                    bytes
                };
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
                    || format["channels"] != 2
                    || !matches!(format["format"].as_str(), Some("f32" | "i16"))
                    || format["channel_mask"] != 3
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
}
